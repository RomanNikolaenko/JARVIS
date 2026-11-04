# Memory Encryption at Rest Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The memory database can be encrypted with SQLCipher from the Memory screen; its recovery code is the key, kept in the OS keychain and shown once, and a locked database can be unlocked with the code.

**Architecture:** rusqlite switches to the bundled SQLCipher build. `jarvis-storage` learns keys, file-state detection, an encrypted copy, a swap of the live connection (all `Database` clones share it) and tidying of interrupted swaps. `MemoryEngine` keeps its database behind a lock so an unlock attaches it without a restart. The core owns the code, the keychain entry and the order of the steps; the desktop exposes three commands and the Memory screen drives them.

**Tech Stack:** Rust 2024 (rusqlite 0.40 with `bundled-sqlcipher-vendored-openssl`, keyring 3 via `SecretStore`, getrandom 0.3), Tauri 2, Angular 22.

**Spec:** `docs/superpowers/specs/2026-11-04-memory-encryption-design.md`

## Global Constraints

- Keychain entry: service `jarvis` (`KEYCHAIN_SERVICE`), key `memory-key`.
- Recovery code: 48 decimal digits as 8 groups of 6 joined by `-`; the confirmation is the last group. Input is normalized to digits only and must have exactly 48.
- SQLCipher 4 defaults (no `cipher_*` pragmas); the code is the passphrase (`PRAGMA key = '<code>'`).
- The code never appears in logs, events, error messages or any IPC response except `prepare_memory_encryption`.
- Free space before encrypting: at least the database size × 1.1 (`jarvis_platform::available_disk_space`).
- Wrong code message: `This recovery code does not open the memory database`.
- No new crates except those the rusqlite feature brings (`openssl-sys`, `openssl-src`); `getrandom` 0.3 (already in `Cargo.lock`) becomes a direct dependency of `jarvis-core`.
- Ask the user before downloading Strawberry Perl (name, source, size).
- Full local CI before every push; push only `main` after a branch check of CI (Task 1).

## Review Focus

1. JARVIS stops between the renames of step 4: the next start opens whichever complete database exists and removes the leftovers (Task 3 tests).
2. A recovery code typed with spaces, without dashes or with extra characters: normalized; 47 or 49 digits are refused (Task 5 test).
3. An agent task running during encryption: encryption is refused like a running reply (Task 5 test).
4. Export, import, "Delete everything" and search after encryption keep working on the encrypted file (Task 5 test).
5. Unlock on a database that is not encrypted: refused with "Memory is not encrypted", nothing written to the keychain (Task 5 test).

---

### Task 1: SQLCipher in the build

**Files:**
- Modify: `Cargo.toml` (`rusqlite = { version = "0.40", features = ["bundled-sqlcipher-vendored-openssl"] }`), `Cargo.lock`
- Modify: `crates/jarvis-storage/src/lib.rs` (test)
- Modify: `.github/scripts/third-party-notices.mjs` (+ regenerated `THIRD-PARTY-NOTICES.txt`)
- Modify: `.github/workflows/ci.yml`, `.github/workflows/release.yml` (Perl for OpenSSL on Windows, only if the branch check needs it)
- Modify: `DEVELOPMENT.md` (prerequisites: Perl; Strawberry Perl on Windows)

**Interfaces:**
- Produces: a SQLCipher-capable `rusqlite` for every crate.

- [ ] **Step 1: Ask the user** to approve downloading Strawberry Perl (official `strawberryperl.com` MSI, current 5.40 64-bit, ~200 MB) and install it; stop until they answer.
- [ ] **Step 2: Write the failing test** `sqlcipher_is_compiled_in` in `jarvis-storage` tests: `Database::open_in_memory()` then `SELECT sqlcipher_version()` / `PRAGMA cipher_version` returns a string starting with `4.14.0`.
- [ ] **Step 3: Run** `cargo test -p jarvis-storage --locked sqlcipher_is_compiled_in` → FAIL (`no such function` / empty).
- [ ] **Step 4: Switch the feature** in `Cargo.toml` (`Cargo.lock` gains `openssl-sys` and `openssl-src`); if the build finds Git's MSYS perl first, set `PERL=C:Strawberryperlinperl.exe` for local builds and note it in DEVELOPMENT.md.
- [ ] **Step 5: Run** `cargo test --workspace --locked` → all pass (plain databases unchanged); record the clean-build time added.
- [ ] **Step 6: Notices:** make the generator also emit `libsqlite3-sys/sqlcipher/LICENSE` (SQLCipher, BSD) and OpenSSL's `LICENSE.txt` from `openssl-src`; `node .github/scripts/third-party-notices.mjs --check` passes after regeneration.
- [ ] **Step 7: Branch CI check:** push to a temporary branch `ci-sqlcipher` and wait for every CI job, including `windows-11-arm` and `ubuntu-24.04-arm`; fix the workflows if a runner lacks Perl (the release workflow uses the same runners and is checked at the next release); delete the branch once it is green.
- [ ] **Step 8: Commit** `build: SQLCipher for the memory database`.

### Task 2: Opening with a key; locked databases

**Files:**
- Modify: `crates/jarvis-storage/src/lib.rs`, `src/error.rs`, `src/migrations.rs` (key passed to `migrate` for backups)

**Interfaces:**
- Produces:
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum FileState { Missing, Plain, Encrypted }` and `pub fn file_state(path: &Path) -> std::io::Result<FileState>` (Missing also for an empty file; Plain iff the first 16 bytes are `b"SQLite format 3\0"`).
  - `Database::open_with_key(path: impl Into<PathBuf>, key: Option<String>) -> Result<Self>`; `Database::open(path)` = `open_with_key(path, None)`.
  - `StorageError::Locked` — "the memory database is encrypted and its key is missing or wrong".
  - `Database::path(&self) -> Option<&Path>`.
- Behaviour: `PRAGMA key` is the first statement when a key is given; an Encrypted file without a key, or with a wrong one (SQLCipher `NotADatabase` on the first read), is `Locked`, never `Corrupt`; a Plain file opened with a key is opened plain (the key is ignored) — `file_state` decides.

- [ ] **Step 1: Write failing tests:** `an_encrypted_database_hides_its_content` (create with key, insert a row containing `"Atlas deadline"`, close; the file does not start with the SQLite header and `std::fs::read` does not contain `"Atlas deadline"`; reopening with the key reads the row; FTS5 compile option still true); `a_missing_or_wrong_key_reports_locked` (open without key and with another code → `StorageError::Locked`); `a_plain_database_ignores_a_key` (plain file + key → opens, still Plain).
- [ ] **Step 2: Run** `cargo test -p jarvis-storage --locked` → FAIL to compile.
- [ ] **Step 3: Implement.** Pass the key through `open_file` → `Connection::open` → `pragma_update(None, "key", key)` before `configure`; map the `NotADatabase` error of an Encrypted file to `Locked`.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `feat(storage): open encrypted databases with a key`.

### Task 3: Encrypted copy, swap and leftovers

**Files:**
- Modify: `crates/jarvis-storage/src/lib.rs` (new `encryption.rs` module for these functions), `src/migrations.rs`

**Interfaces:**
- Consumes: Task 2.
- Produces:
  - `Database::export_encrypted(&self, key: &str) -> Result<()>` — spec §4 steps 1–2 into `<db>.encrypting` (row counts of every table in `sqlite_master` compared; mismatch deletes the copy and returns `Internal("the encrypted copy is incomplete")`).
  - `Database::discard_encrypted_copy(&self)` — deletes `<db>.encrypting` (used when step 3 fails).
  - `Database::switch_to_encrypted(&self, key: &str) -> Result<()>` — spec §4 steps 4–5 under the connection mutex: swap in `Connection::open_in_memory()` to close the file, rename, open `<db>` with the key (+ `configure`), store it in the mutex, then wipe (zero-fill, then delete) `<db>.plain-old`, `<db>.plain-old-wal`, `<db>.plain-old-shm`, `<db>-wal`/`-shm` left from the plain file, and every `<name>.v*-*.bak` next to it that is Plain.
  - `fn tidy_leftovers(path: &Path) -> Result<()>` called first by `open_with_key` (spec §4 rules).
  - Migration backups of an Encrypted database use `ATTACH … KEY` + `sqlcipher_export` (backup file Encrypted).
- [ ] **Step 1: Write failing tests:** `encrypting_keeps_every_row_and_search` (plain db with a conversation, message, fact, agent task row; export + switch; reopen with key: same counts, FTS `MATCH` finds the message; `file_state` Encrypted; no `.plain-old`, `.encrypting` or plain `.bak` remain); `a_stop_before_the_swap_keeps_the_plain_database` (export only, drop, reopen without key → Plain opens; `.encrypting` gone); `a_stop_after_the_swap_wipes_the_plain_copy` (simulate: encrypted `<db>` + a plain `<db>.plain-old` file → open with key succeeds and `.plain-old` is gone); `backups_of_an_encrypted_database_are_encrypted` (encrypted db at schema 4 via `migrate(&MIGRATIONS[..4])` with key, migrate to all: `report.backup` exists and is Encrypted).
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** (Windows: every rename happens with no open handle; use `std::fs::rename`, which replaces on the same volume).
- [ ] **Step 4: Run** `cargo test -p jarvis-storage --locked` → PASS.
- [ ] **Step 5: Commit** `feat(storage): encrypt a database in place, safe against interruption`.

### Task 4: Memory engine

**Files:**
- Modify: `crates/jarvis-memory/src/engine.rs`, `src/types.rs` (status), callers of `database()` in `crates/jarvis-core`
- Test: `crates/jarvis-memory/tests/engine.rs`

**Interfaces:**
- Consumes: Tasks 2–3.
- Produces:
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)] #[serde(rename_all = "camelCase")] #[ts(export_to = "memory.ts")] pub enum MemoryEncryption { Off, On, Locked, Unsupported }`; `MemoryStatus.encryption: MemoryEncryption`.
  - `MemoryEngine::open_with_key(path, key: Option<String>) -> Self` (`open(path)` = no key); the engine stores the path; `db` becomes `std::sync::RwLock<Option<Database>>`, `unavailable` likewise; `database(&self) -> Option<Database>` (owned clone).
  - `MemoryEngine::encryption(&self) -> MemoryEncryption` (Locked when the last open failed with `Locked`; otherwise from `file_state`).
  - `MemoryEngine::unlock(&self, key: String) -> Result<()>` — opens with the key; `Locked` → `MemoryError::Invalid("This recovery code does not open the memory database")`; a Plain file → `MemoryError::Invalid("Memory is not encrypted")`; success attaches the database and clears `unavailable`.
- [ ] **Step 1: Write failing tests:** `a_locked_engine_unlocks_with_its_key` (encrypted file, `open_with_key(None)` → unavailable, `encryption()==Locked`; `unlock(wrong)` → Invalid; `unlock(right)` → conversations listed, `encryption()==On`); `unlock_refuses_a_plain_database`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement;** adjust the core's `memory.database()` call sites to the owned return.
- [ ] **Step 4: Run** `cargo test -p jarvis-memory -p jarvis-core --locked` → PASS.
- [ ] **Step 5: Commit** `feat(memory): encrypted, locked and unlocked memory databases`.

### Task 5: Core: key, encryption, unlock

**Files:**
- Modify: `crates/jarvis-core/src/memory.rs` (or new `src/memory_encryption.rs`), `src/lib.rs` (key at start), `Cargo.toml` (`getrandom`)
- Test: `crates/jarvis-core/tests/memory_encryption.rs`

**Interfaces:**
- Consumes: Task 4; `SecretStore` (`get_secret`, `set_secret`).
- Produces:
  - `pub const MEMORY_KEY_SECRET: &str = "memory-key";`
  - `#[derive(Serialize, TS)] #[ts(export_to = "core.ts")] pub struct MemoryRecoveryCode { pub code: String }`
  - `JarvisCore::prepare_memory_encryption(&self) -> Result<MemoryRecoveryCode, CoreError>` — needs saving on, `encryption()==Off`, a working keychain (else `Unavailable`); keeps the code in `MemoryState`.
  - `JarvisCore::encrypt_memory(&self, confirmation: String) -> Result<(), CoreError>` — refuses: no prepared code, confirmation ≠ last group, a reply or an agent task running ("Stop the running replies and agent tasks before encrypting memory"), not enough space; then export → `set_secret` (failure → `discard_encrypted_copy`, error) → switch; clears the prepared code; Info event "Memory is encrypted".
  - `JarvisCore::unlock_memory(&self, code: String) -> Result<(), CoreError>` — normalize; 48 digits else `Validation`; `memory.unlock`; `set_secret`; Info event "Memory was unlocked; agent tasks use it after the next start".
  - At start: `get_secret(MEMORY_KEY_SECRET)` in `spawn_blocking`; a keychain error makes the status `Unsupported` (when the file is not Encrypted) and opens without a key.
- [ ] **Step 1: Write failing tests** (`InMemorySecretStore`; a `FailingSecretStore` whose `set_secret` errs):
  - `encrypting_needs_the_last_group_and_keeps_memory_working` — save a conversation and fact; prepare (code matches `^\d{6}(-\d{6}){7}$`); a wrong group is refused; the right one encrypts; `memory_status().encryption == On`; the keychain holds the code; search, export, clear still work; no `SystemEvent` message contains the code.
  - `a_restart_without_the_key_is_locked_and_the_code_unlocks_it` — encrypt, restart with an empty secret store → status Locked and memory unavailable; `unlock_memory(" 123… with spaces/no dashes ")` normalized works; 47 digits → Validation; the store holds the code afterwards.
  - `a_failing_keychain_leaves_the_plain_database` — FailingSecretStore → error, database still Plain with its data, no `.encrypting` file.
  - `encryption_waits_for_replies_and_agent_tasks` — a gated reply → refused; a running agent task → refused.
  - `unlock_refuses_a_plain_database` — Plain → Invalid "Memory is not encrypted", store untouched.
- [ ] **Step 2: Run** `cargo test -p jarvis-core --test memory_encryption --locked` → FAIL.
- [ ] **Step 3: Implement.** Code generation: draw bytes with `getrandom::fill`, keep bytes `< 250`, take `byte % 10` until 48 digits.
- [ ] **Step 4: Run** the file and `cargo test -p jarvis-core --locked` → PASS.
- [ ] **Step 5: Commit** `feat(core): encrypt memory with a recovery code kept in the keychain`.

### Task 6: Desktop IPC

**Files:**
- Modify: `apps/desktop/src-tauri/src/memory_commands.rs`, `src/lib.rs`, `build.rs`, `capabilities/main-window.json`, `permissions/autogenerated/*` (build), `src/ipc_tests.rs`, `src/bindings.rs` (`MemoryRecoveryCode`, `MemoryEncryption`)

**Interfaces:**
- Produces commands `prepare_memory_encryption() -> MemoryRecoveryCode`, `encrypt_memory(confirmation: String)`, `unlock_memory(code: String)`.
- [ ] **Step 1: Failing tests:** add the three to the denied list; `memory_encryption_over_ipc` (in-memory store harness): `get_memory_status` has `encryption: "off"`; `unlock_memory` with a plain db → `invalidInput`; `encrypt_memory` without prepare → `validation`.
- [ ] **Step 2–4:** run → FAIL; implement; `cargo test -p jarvis-desktop --locked` → PASS (bindings regenerated).
- [ ] **Step 5: Commit** `feat(desktop): memory encryption commands`.

### Task 7: Memory screen

**Files:**
- Modify: `apps/desktop/src/app/core/ipc/jarvis-api.ts`, `core/state/memory.store.ts`, `features/memory/data-panel/*`, `features/memory/memory.html` (locked notice), specs `memory.store.spec.ts`, `features/memory/memory.spec.ts`

**Interfaces:**
- Consumes: Task 6.
- Produces: store `prepareEncryption(): Promise<string | null>`, `encrypt(confirmation)`, `unlock(code)`; copy as in spec §6.
- [ ] **Step 1: Failing specs:** the status line for `off`/`on`/`locked`/`unsupported`; **Encrypt memory…** shows the code in 8 groups and **Encrypt** stays disabled until the typed group equals the last group, then calls `encryptMemory(group)`; the locked form calls `unlockMemory(code)` and shows the error message; the export button shows the plain-file note when `on`; the button is disabled with the keychain reason when `unsupported`.
- [ ] **Step 2–4:** run `npm run test:ci` → FAIL; implement; all frontend gates pass.
- [ ] **Step 5: Commit** `feat(ui): encrypt and unlock memory on the Memory screen`.

### Task 8: Documentation, live check, release gate

**Files:**
- Modify: `ARCHITECTURE.md`, `SECURITY.md`, `docs/decisions/0009-local-memory.md` (addendum), `CHANGELOG.md`, `ROADMAP.md`, the spec (agents after unlock: next start)

- [ ] **Step 1: Live check** (test profile with conversations/facts, real keychain): encrypt from the UI; first bytes of `jarvis.db` ≠ SQLite header and a known phrase is absent (`Select-String`); chat with recall and search work; delete `jarvis/memory-key` in Credential Manager (`cmdkey /delete:` or the keyring API) → restart → Locked; unlock with the code → works; remove the test entry and profile afterwards.
- [ ] **Step 2: Docs** with what was observed.
- [ ] **Step 3: Full local CI** → 11/11.
- [ ] **Step 4: Commit and push; GitHub CI green.**

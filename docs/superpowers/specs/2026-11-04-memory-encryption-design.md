# Memory: encryption at rest

- Date: 2026-11-04
- Status: approved design (implementation pending)
- Area: workspace `Cargo.toml` (rusqlite features), `crates/jarvis-storage`,
  `crates/jarvis-memory`, `crates/jarvis-core` (memory, IPC), Memory screen,
  CI and release workflows, third-party notices

## Goals

The memory database (`<data>/memory/jarvis.db`: conversations, facts,
suggestions, projects, the action history, agent journals and the audit log)
can be encrypted on disk, with its key in the OS keychain (ROADMAP Phase 4).
Someone who copies the file — a stolen laptop, a disk image, a backup,
another account — cannot read it. On the development machine BitLocker is off
on both drives, so today the file is readable as is.

Not protected against (stated in the UI and SECURITY.md): programs running as
the user (they can read the keychain like JARVIS), memory exports (plain JSON
files the user creates on purpose), and old plaintext blocks a solid-state
drive may keep after the plaintext file is deleted.

Non-goals: turning encryption off again, changing the key, encrypting
settings, logs or models, automatic encryption without the user's action.

## Decisions taken with the user

1. SQLCipher, with the key in the OS keychain.
2. A recovery code on paper opens the database when the keychain entry is
   gone.
3. Encryption starts with a button on the Memory screen, never on its own.

## Design

### 1. Build

- `rusqlite` uses `bundled-sqlcipher-vendored-openssl` instead of `bundled`:
  libsqlite3-sys 0.38.2 bundles SQLCipher 4.14.0 community (SQLite 3.51.3,
  FTS5 enabled by the same build flags) and builds OpenSSL from source through
  `openssl-sys/vendored`. A database opened without a key behaves as plain
  SQLite, so existing databases keep working unchanged.
- Building OpenSSL needs Perl (a native Windows Perl on Windows, e.g.
  Strawberry Perl) and a C compiler. GitHub's Windows and Ubuntu images have
  both; the ARM64 runners are checked on a branch before `main`. The
  development machine needs Strawberry Perl installed — a download asked for
  separately.
- `THIRD-PARTY-NOTICES.txt` gains SQLCipher's BSD licence
  (`libsqlite3-sys/sqlcipher/LICENSE`) and OpenSSL's Apache-2.0 licence (from
  `openssl-src`), which the generator does not find on its own.

### 2. The key

- The key is the **recovery code**: 48 decimal digits in 8 groups of 6
  (`123456-…`, about 159 bits), from the OS random source (`getrandom`, already
  in the lock file). It is SQLCipher's passphrase (SQLCipher 4 defaults:
  AES-256, PBKDF2-HMAC-SHA512 with 256 000 iterations), so no other crypto
  code or key wrapping is needed.
- The keychain holds the same code under service `jarvis`, key `memory-key`.
- The code never reaches logs, events, error messages or IPC responses other
  than the one that shows it before encrypting.

### 3. Opening (`jarvis-storage`)

- `Database::open(path, key: Option<&str>)` sets `PRAGMA key` before any other
  statement when a key is given.
- The first 16 bytes tell the file's state: `SQLite format 3\0` is plain; any
  other non-empty content is treated as encrypted. A new database is created
  plain.
- States reported up to the UI: `off` (plain), `on` (encrypted and open),
  `locked` (encrypted; no key or a wrong one — memory is unavailable as with a
  corrupt file, temporary chats still work), `unsupported` (no keychain).
- A wrong key is recognised by SQLCipher's "file is not a database" on the
  first read and reported as `locked`, never as corruption.
- Backups before migrations of an encrypted database are encrypted with the
  same key (`sqlcipher_export`), never plain.

### 4. Encrypting an existing database

`JarvisCore::encrypt_memory(confirmation)` runs only when no reply is running
(as `clear_memory`), after checking free space (at least the database size
plus 10 %):

1. Store the confirmed code in the keychain. A failure stops the operation
   before it writes an encrypted copy.
2. `PRAGMA wal_checkpoint(TRUNCATE)`; `ATTACH '<db>.encrypting' AS enc KEY
   <code>`; `SELECT sqlcipher_export('enc')`; `DETACH`.
3. Open `<db>.encrypting` with the code and compare the row counts of every
   table with the plain database.
4. Close the plain database; rename it to `<db>.plain-old`; rename
   `<db>.encrypting` to `<db>`; open it with the key.
5. Overwrite with zeros and delete `<db>.plain-old`, its `-wal`/`-shm` files
   and the plain migration backups (`*.bak` next to it).

Steps 2–4 hold the shared database connection lock. Writes accepted while
the keychain waits are included in the export. Writes queued during the
swap use the encrypted connection afterwards.

On startup, an abandoned `<db>.encrypting` beside the original plain database
is removed. When a swap was interrupted, the old database and backups remain
until the replacement opens with the key and passes the schema and integrity
checks. A copy that still depends on its write-ahead log stays in place for
recovery. A key saved before a failed conversion is ignored while the
database is plain.

The recovery code is generated by `prepare_memory_encryption()` and kept in
core memory only until `encrypt_memory` succeeds or JARVIS closes; each call
replaces it. The confirmation is the code's last group, typed by the user.

### 5. Unlocking

`JarvisCore::unlock_memory(code)`: normalizes the code (digits only, 48 of
them), opens the database with it, and on success stores it in the keychain
and makes memory available without a restart. A wrong code returns
`invalidInput` "This recovery code does not open the memory database". No
attempt limit: every try costs the key derivation (about 0.2 s) and the code
has about 159 bits.

### 6. Interface (Memory → Data & settings)

- Status line: "Memory is not encrypted" / "Encrypted (key in the system
  keychain)" / "Locked" / "Encryption needs the system keychain, which is not
  available".
- Not encrypted: **Encrypt memory…** opens a dialog with the recovery code
  (monospace, 8 groups, **Copy**), the warning "Without this code and without
  the keychain entry the memory cannot be recovered", a field for the last
  group, and **Encrypt** (enabled when the group matches). A spinner while it
  runs, then the encrypted status.
- Locked (Memory screen shows the reason, as for an unavailable database): a
  field "Recovery code" and **Unlock**.
- Encrypted: the export button notes that an export is a plain file.
- IPC: `prepare_memory_encryption() -> { code }`,
  `encrypt_memory(confirmation)`, `unlock_memory(code)`; `MemoryStatus` gains
  `encryption: "off" | "on" | "locked" | "unsupported"`.

### 7. Errors

Wrong code (above); keychain unavailable or failing (`unsupported`, or the
encryption stops at step 3); not enough space; a reply running; no prepared
code or a mismatched confirmation. Logs carry the failing step, never the code or
message text.

## Testing

- `jarvis-storage`: an encrypted database has no plain header and its text
  is not found in the file; a wrong key reports locked; encrypting keeps
  every table and FTS search; the backup before a migration is encrypted;
  leftovers after a stop at each step are tidied and the database opens.
- `jarvis-core` with an in-memory secret store: prepare/encrypt/unlock;
  locked at start without the key; a failing keychain stops at step 1 with
  the plain database intact; replies running refuse encryption; a wrong
  confirmation or code is refused; the code never appears in events.
- Desktop IPC: the three commands and the status field.
- Frontend: the dialog's confirmation gating, the locked form, status lines.
- Live (Windows, test profile with conversations and facts): encrypt; the
  file's first bytes are not the SQLite header and a known phrase is absent;
  chat, recall and search work; delete the `jarvis/memory-key` entry in
  Credential Manager → locked; unlock with the code → works again.

## Documentation

ARCHITECTURE (storage, memory, limitations "not encrypted at rest"),
SECURITY.md, ADR 0009 addendum, DEVELOPMENT (Perl prerequisite), CHANGELOG,
ROADMAP Phase 4.

# Mentor Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A voice mentor that listens continuously, teaches the owner while
they build an app, sees their project files (and the screen on request),
speaks up when they save, and types confirmed code into files slowly while
explaining it.

**Architecture:** Approach B of the spec. The voice engine gets a
`Conversation` listening mode. `jarvis-core` gets a `mentor` module that owns
the session (project folder, pause, proactive guards), builds each turn's
hidden project context, parses the mentor's `<plan>` and `<write>` blocks
after the reply is saved, asks for approval and types the change. The
desktop forwards IPC and events; an Angular `MentorStore` routes
conversation transcripts, voice approvals and nudges to chat turns, in the
existing voice-store pattern.

**Tech Stack:** Rust (tokio, rusqlite/SQLCipher, ts-rs), Tauri 2, Angular 22
(signals, `ng test`).

**Spec:** `docs/superpowers/specs/2026-11-13-mentor-mode-design.md`

## Global Constraints

- No new dependencies (no `notify`, no diff crate, no `walkdir` as a direct dependency: use `std::fs::read_dir`).
- Every write goes through `PolicyEngine::evaluate` and `ApprovalBroker::request`, is audited, and stays inside workspace folders (`FileScope`).
- A cloud mentor needs `privacy.allowCloudProviders` + `privacy.allowCloudAgents` for files and `privacy.shareCapturesWithCloud` for screenshots; otherwise `mentor_start` fails naming the missing switch.
- Microphone: opening and closing are logged (`EventCategory::Voice`); `VoiceState.micOpen` drives the indicator.
- Snapshot limits: skip `.git`, `node_modules`, `target`, `dist`, `build`, `out`, `.next`, `.angular`, `bin`, `obj`, `.venv`, `__pycache__`; text files only; files over 256 KiB listed by name only; at most 5 000 files scanned.
- Changed-file budget: 30 % of the context window (chars ≈ tokens × 3), most recently modified first; the rest by name.
- Proactive turns: files settled for 2 s; at most one per 20 s; none while speaking, answering, holding, typing or paused; after 3 in a row without an owner phrase, none until the owner speaks; JARVIS's own writes never count.
- Typing: write to disk about every 150 ms, a few lines (at most 3) per write; final content must equal the approved result.
- Spoken answers: the mentor prompt asks for at most 4 sentences and one step at a time, in the assistant language.
- Voice commands (Ukrainian and English, case- and punctuation-insensitive, whole phrase): pause «пауза» / "pause"; end «закінчуємо урок», «кінець уроку» / "end the lesson"; look «подивись на екран», «що на екрані» / "look at the screen"; yes «так», «пиши», «давай» / "yes", "write it"; no «ні», «не треба» / "no"; stop «стоп» / "stop". Resume only by the button or push-to-talk («продовжуємо» / "continue" then also accepted).
- Repository rules: docs in English, full local CI before every push, update ARCHITECTURE, CHANGELOG, ROADMAP and SECURITY with the change, no `unwrap()` in non-test code without a reason.

## Review Focus

1. The owner edits the file while JARVIS types (unsaved buffer saved mid-typing): typing stops, the owner's text stays, the mentor is told. → Task 8 test `typing_stops_when_the_owner_edits_the_file`.
2. JARVIS hears its own voice in conversation mode (no headset): the echo gate must keep its speech from becoming a phrase. → Task 1 test `conversation_mode_ignores_own_speech`.
3. A huge or binary file, or a project with thousands of files: no stall, no non-text content in the prompt. → Task 4 test `skips_large_binary_and_ignored_files`.
4. A `<write>` path outside the project or a `find` fragment that is not unique: refused before any approval is asked. → Task 5 test `rejects_paths_outside_and_ambiguous_fragments` and Task 8 test `refused_write_asks_nothing`.
5. The owner talks while JARVIS is still thinking: phrases are held and sent together after the reply, never lost and never sent twice. → Task 11 test `holds_phrases_until_the_reply_ends`.

---

### Task 1: Conversation listening mode in the voice engine

**Files:**
- Modify: `crates/jarvis-voice/src/types.rs` (`TranscriptSource`, `VoiceState`)
- Modify: `crates/jarvis-voice/src/engine.rs` (`ListenMode`, listener loop, public API)
- Test: `crates/jarvis-voice/src/engine.rs` tests module (uses `testing::FakeBackend`)

**Interfaces:**
- Produces: `TranscriptSource::Conversation`; `VoiceState.conversation_active: bool`; `VoiceEngine::start_conversation(&self) -> Result<()>` (fails like `listen()` when no speech model); `VoiceEngine::stop_conversation(&self)`; `VoiceEngine::say_and_wait(&self, text: &str) -> impl Future<Output = ()>` (resolves when that speech ends or is stopped).

- [ ] **Step 1: Write failing tests** `conversation_mode_emits_every_phrase` (two phrases without the wake word → two transcripts with source `Conversation`, mic stays open, `conversation_active == true`), `conversation_mode_ignores_own_speech` (a phrase captured while `reply_playing` and below the echo gate threshold produces no transcript; one louder than the gate stops speech and is emitted), `stop_conversation_closes_the_microphone` (`mic_open == false`, wake word mode restored if `settings.wake_word`), `say_and_wait_resolves_after_playback`.
- [ ] **Step 2: Run** `cargo test -p jarvis-voice conversation` → FAIL (no such API).
- [ ] **Step 3: Implement.** Add `ListenMode::Conversation`. It reuses the wake-word worker path but the worker emits every transcribed phrase with `has_words` as `Conversation` (no wake word, no probe). Barge-in watching is always on in this mode (ignore `interrupt_by_voice`). `refresh_wake_word` must not replace a running `Conversation` listener; `stop_conversation` restarts the wake listener when the setting is on. `listen()` while in conversation mode returns `Ok(())` without starting push-to-talk. Log mic open/close as the other modes do.
- [ ] **Step 4: Run** `cargo test -p jarvis-voice` → PASS.
- [ ] **Step 5: Commit** `feat(voice): conversation listening mode`.

### Task 2: Keep mentor blocks out of speech

**Files:**
- Modify: `crates/jarvis-voice/src/text.rs` (sentence filter `take`)
- Test: same file's tests

**Interfaces:**
- Produces: lines between `<plan>` … `</plan>` and `<write …>` … `</write>` are never spoken, even when the tags arrive split across deltas, like `<think>`.

- [ ] **Step 1: Write failing test** `skips_plan_and_write_blocks`: pushing `"Добре.\n<plan>\n1. крок\n</plan>\n<write path=\"a.ts\" mode=\"create\">\n<say>Імпорт.</say>\n<code>\nimport x\n</code>\n</write>\nДалі."` in 5-character pieces yields exactly `["Добре.", "Далі."]`.
- [ ] **Step 2: Run** `cargo test -p jarvis-voice skips_plan` → FAIL.
- [ ] **Step 3: Implement** with an `in_block: Option<&'static str>` that tracks the closing tag, alongside `in_think`.
- [ ] **Step 4: Run** `cargo test -p jarvis-voice` → PASS.
- [ ] **Step 5: Commit** `feat(voice): do not speak mentor plan and write blocks`.

### Task 3: Mentor sessions in storage

**Files:**
- Create: `crates/jarvis-storage/migrations/0006_mentor.sql`
- Modify: `crates/jarvis-storage/src/lib.rs` (`MIGRATIONS` list), `crates/jarvis-memory/src/{types.rs,store.rs,engine.rs}`
- Test: `crates/jarvis-memory/src/store.rs` tests

**Interfaces:**
- Produces: table `mentor_sessions (conversation_id TEXT PRIMARY KEY REFERENCES conversations(id) ON DELETE CASCADE, project_folder TEXT NOT NULL, created_at INTEGER NOT NULL)`; `MemoryEngine::set_mentor_session(&self, conversation_id: Uuid, project_folder: &Path) -> Result<()>`; `MemoryEngine::mentor_session(&self, conversation_id: Uuid) -> Result<Option<MentorSession>>`; `pub struct MentorSession { pub conversation_id: Uuid, pub project_folder: PathBuf, pub created_at_ms: i64 }` (ts-rs, `memory.ts`). Temporary conversations keep the link in memory only, like their messages.

- [ ] **Step 1: Write failing tests** `mentor_session_round_trip`, `mentor_session_goes_with_its_conversation` (delete conversation → `None`), `migration_6_upgrades_an_existing_database` (schema 5 → 6 keeps rows).
- [ ] **Step 2: Run** `cargo test -p jarvis-memory mentor` → FAIL.
- [ ] **Step 3: Implement** the migration, store functions and engine wrappers; regenerate bindings (`cargo test -p jarvis-memory export_bindings` as the crate does today).
- [ ] **Step 4: Run** `cargo test -p jarvis-storage -p jarvis-memory` → PASS.
- [ ] **Step 5: Commit** `feat(memory): mentor sessions`.

### Task 4: Project snapshot and change set

**Files:**
- Create: `crates/jarvis-core/src/mentor/snapshot.rs`, `crates/jarvis-core/src/mentor/mod.rs` (module root only)
- Test: `snapshot.rs` tests (tempdir)

**Interfaces:**
- Produces: `ProjectSnapshot::scan(root: &Path) -> io::Result<ProjectSnapshot>` (blocking; callers use `spawn_blocking`); `ProjectSnapshot::changes_since(&self, older: &ProjectSnapshot) -> ChangeSet`; `ChangeSet { created: Vec<PathBuf>, changed: Vec<PathBuf>, deleted: Vec<PathBuf> }` with relative paths and `is_empty()`; `ChangeSet::render(&self, root: &Path, budget_chars: usize) -> String`; `ProjectSnapshot::tree(&self, max_entries: usize) -> String`; `ProjectSnapshot::mark_written(&mut self, path: &Path)` (records JARVIS's own write so it is not a change).

- [ ] **Step 1: Write failing tests** `reports_created_changed_and_deleted_files`, `skips_large_binary_and_ignored_files` (a 300 KiB file listed by name only, a file with a NUL byte skipped, `node_modules/x.js` absent, 6 000 files → scan stops at 5 000 without error), `render_respects_the_budget_newest_first` (two changed files, budget fits one → newest full text, the other `- path (not shown)`), `own_writes_are_not_changes`.
- [ ] **Step 2: Run** `cargo test -p jarvis-core mentor::snapshot` → FAIL.
- [ ] **Step 3: Implement.** Entry = (size, mtime). Text detection: valid UTF-8 and no NUL in the first 8 KiB. `render` wraps every file in `<project_file path="…">…</project_file>` and starts with one line saying the content is untrusted data from the owner's project.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `feat(core): mentor project snapshot`.

### Task 5: Mentor reply protocol and voice commands

**Files:**
- Create: `crates/jarvis-core/src/mentor/protocol.rs`, `crates/jarvis-core/src/mentor/commands.rs`
- Test: both files' tests

**Interfaces:**
- Produces: `parse_reply(text: &str) -> MentorReply`; `MentorReply { plan: Option<String>, write: Option<Result<WriteBlock, String>> }`; `WriteBlock { path: String, change: WriteChange, parts: Vec<WritePart> }`; `WriteChange::Create | WriteChange::Replace { find: String }`; `WritePart { say: String, code: String }`; `WriteBlock::code(&self) -> String` (parts joined). `parse_command(text: &str) -> Option<MentorCommand>`; `MentorCommand::{Pause, Resume, End, Look, Yes, No, Stop}`.
- Format (the prompt in Task 7 teaches it):
  ```
  <write path="src/app.ts" mode="create|replace">
  <find>exact existing text</find>        (replace only)
  <say>short explanation</say>
  <code>
  lines
  </code>
  … more say/code pairs …
  </write>
  ```

- [ ] **Step 1: Write failing tests** `parses_plan_and_create_block`, `parses_replace_block_with_find`, `rejects_paths_outside_and_ambiguous_fragments` (absolute path, `..`, empty `find`, missing `<code>` → `Some(Err(reason))`; uniqueness against the file is checked in Task 8), `only_the_last_plan_counts`, `commands_match_whole_phrases` («Пауза.» → Pause; «пауза в коді» → None; "Look at the screen!" → Look; «так» → Yes).
- [ ] **Step 2: Run** `cargo test -p jarvis-core mentor::protocol mentor::commands` → FAIL.
- [ ] **Step 3: Implement** with plain string scanning (no regex dependency unless the crate already has one); `<code>` content keeps its newlines exactly, minus the first and last newline.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `feat(core): mentor reply protocol and voice commands`.

### Task 6: Mentor session lifecycle

**Files:**
- Modify: `crates/jarvis-core/src/mentor/mod.rs`, `crates/jarvis-core/src/lib.rs` (field `mentor: MentorState`, shutdown)
- Test: `crates/jarvis-core/src/mentor/mod.rs` tests (core test helpers used by `memory.rs` tests)

**Interfaces:**
- Consumes: Task 1 API, Task 3 `set_mentor_session` / `mentor_session`, Task 4 `ProjectSnapshot`.
- Produces: `JarvisCore::mentor_start(&self, request: MentorStartRequest) -> Result<MentorStatus, CoreError>` with `MentorStartRequest { conversation_id: Uuid, project_folder: String, provider_id: Option<ProviderId>, model: Option<String> }`; `mentor_stop(&self)`; `mentor_pause(&self)`; `mentor_resume(&self) -> Result<MentorStatus, CoreError>`; `mentor_status(&self) -> MentorStatus`; `subscribe_mentor(&self) -> watch::Receiver<MentorStatus>`; `MentorStatus { conversation_id: Option<Uuid>, project_folder: Option<String>, phase: MentorPhase, pending_write: Option<String>, typing_file: Option<String>, last_error: Option<String> }` and `MentorPhase::{Off, Listening, Paused, Typing}` (ts-rs, `core.ts`). One session at a time.

- [ ] **Step 1: Write failing tests** `start_requires_a_workspace_folder` (folder outside `tools.workspaceFolders` → `Validation`), `cloud_mentor_needs_file_and_cloud_permissions` (cloud provider without `allowCloudAgents` → `PrivacyBlocked` naming the switch), `start_without_speech_model_fails_with_settings_hint`, `start_without_a_voice_runs_text_only` (no Piper voice → starts, `MentorStatus.last_error` says replies are text only), `start_pause_resume_stop` (phases and voice `conversation_active` follow), `second_start_replaces_the_first`.
- [ ] **Step 2: Run** `cargo test -p jarvis-core mentor::tests` → FAIL.
- [ ] **Step 3: Implement.** Start: validate folder (canonical, inside a workspace folder), privacy for the resolved model, save the session link, take the first snapshot (`spawn_blocking`), `voice.start_conversation()`, log `EventCategory::Voice` "Mentor started". Stop also on `JarvisCore::shutdown` and when the conversation is deleted.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `feat(core): mentor sessions start, pause and stop`.

### Task 7: Mentor turns: hidden context, prompt, plan

**Files:**
- Create: `crates/jarvis-core/src/mentor/prompt.rs`
- Modify: `crates/jarvis-core/src/memory.rs` (`ChatTurnRequest`, `send_message`, the reply-saved task), `crates/jarvis-core/src/mentor/mod.rs`
- Test: `crates/jarvis-core/src/mentor/mod.rs` tests with a recording fake provider

**Interfaces:**
- Consumes: Task 4, Task 5 `parse_reply`, Task 6 state.
- Produces: `ChatTurnRequest.mentor: Option<MentorTrigger>` (`#[serde(default)]`); `MentorTrigger::{Phrase, FilesSaved}`; `mentor::after_reply(core, conversation_id, text)` called after a reply of a mentor turn is saved (`Completed` only).

- [ ] **Step 1: Write failing tests** `mentor_turn_adds_changes_but_saves_only_the_phrase` (the provider sees the mentor prompt, a file tree on the first turn and `<project_file>` blocks; the stored user message is just the phrase), `files_saved_turn_is_stored_as_a_short_note` (stored text `[Saved: src/a.ts]`, model told to react briefly), `plan_block_becomes_the_pinned_note`, `non_mentor_turns_are_unchanged`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement.** The prompt (English, answering in the assistant language) states: teacher role, one step at a time, ≤ 4 spoken sentences, never read code aloud, keep the plan in `<plan>`, the exact `<write>` format and that it is used only when the owner asks JARVIS to write. Context goes into the current user message sent to the model only; budget from the resolved model's window (4 096 tokens when unknown). Advance the turn's baseline snapshot after the turn is built.
- [ ] **Step 4: Run** `cargo test -p jarvis-core` → PASS.
- [ ] **Step 5: Commit** `feat(core): mentor turns with project context`.

### Task 8: Confirmed slow typing

**Files:**
- Create: `crates/jarvis-core/src/mentor/typing.rs`
- Modify: `crates/jarvis-core/src/mentor/mod.rs` (`after_reply` hands a write block to typing)
- Test: `typing.rs` tests (tempdir, paused time, auto-approving and denying approval brokers)

**Interfaces:**
- Consumes: Task 5 `WriteBlock`, `jarvis_tools` `write_file` / `edit_file` `prepare` for scope and the `ActionRequest`, `PolicyEngine::evaluate`, `ApprovalBroker::request`, audit log, Task 1 `say_and_wait`, Task 4 `mark_written`.
- Produces: `MentorStatus.pending_write` while approval waits, `MentorPhase::Typing` and `typing_file` while typing; `JarvisCore::mentor_stop_typing(&self)`.

- [ ] **Step 1: Write failing tests** `approved_create_is_typed_in_steps` (several intermediate writes ≥ 150 ms apart, final file equals `WriteBlock::code()`), `replace_types_only_the_new_fragment`, `refused_write_asks_nothing` (policy deny, outside scope, or `find` not exactly once → no approval request, mentor told why), `denied_write_leaves_the_file`, `typing_stops_when_the_owner_edits_the_file` (modify the file between steps → stop, owner's bytes kept, status error says so), `stop_command_keeps_what_was_typed`, `asks_to_save_before_typing` (the first spoken text asks the owner to save the file and not type in it), `overwrite_is_marked_high_risk`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement.** Always ask approval (even when policy allows); `details` holds the full change. Typing loop per part: `say_and_wait(say)`, then write `prefix + typed lines` at most 3 lines per write every 150 ms; before each write compare size, mtime and hash with the last write. Audit `Approved`, then `Succeeded` / `Failed` with the stop reason. After the end tell the conversation (as an assistant note in memory) where typing stopped when it did not finish.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `feat(core): mentor types confirmed code slowly`.

### Task 9: Speaking up on save

**Files:**
- Create: `crates/jarvis-core/src/mentor/watch.rs`
- Modify: `crates/jarvis-core/src/mentor/mod.rs`
- Test: `watch.rs` tests (paused time, tempdir)

**Interfaces:**
- Produces: `JarvisCore::subscribe_mentor_nudges(&self) -> broadcast::Receiver<MentorNudge>`; `MentorNudge { conversation_id: Uuid, files: Vec<String> }` (ts-rs); `JarvisCore::mentor_note_owner_spoke(&self)` and `mentor_set_busy(&self, busy: bool)` (frontend reports answering/speaking/holding).

- [ ] **Step 1: Write failing tests** `nudges_after_files_settle_for_two_seconds`, `no_nudge_while_busy_paused_or_typing`, `at_most_one_nudge_per_twenty_seconds` (changes in between are gathered), `three_nudges_without_a_phrase_then_quiet_until_the_owner_speaks`, `own_writes_never_nudge`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** a 1 s poll task started with the session and cancelled on stop; it compares `(size, mtime)` only and keeps its own baseline separate from the turn baseline.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `feat(core): mentor speaks up when files are saved`.

### Task 10: Desktop IPC and events

**Files:**
- Modify: `apps/desktop/src-tauri/src/lib.rs` (handlers, event forwarding), create `apps/desktop/src-tauri/src/mentor_commands.rs`, `apps/desktop/src-tauri/src/bindings.rs`
- Modify: `packages/shared-types` (generated), `apps/desktop/src/app/core/ipc/jarvis-api.ts`
- Test: `apps/desktop/src-tauri` command tests as existing commands do; `jarvis-api` spec

**Interfaces:**
- Produces: commands `mentor_start(request)`, `mentor_stop`, `mentor_pause`, `mentor_resume`, `mentor_status`, `mentor_stop_typing`, `mentor_report_busy(busy)`, `mentor_owner_spoke`; events `jarvis://mentor-state` (`MentorStatus`), `jarvis://mentor-nudge` (`MentorNudge`); `send_message` accepts `mentor`. Each command gets the 30 s IPC deadline pattern used by chat commands.

- [ ] **Step 1: Write failing tests** for the new `JarvisApi` methods (invoke names and payloads) and event subscriptions.
- [ ] **Step 2: Run** `npm run test:ci` in `apps/desktop` → FAIL.
- [ ] **Step 3: Implement**; regenerate bindings; `git diff --exit-code -- packages/shared-types` clean after commit.
- [ ] **Step 4: Run** `cargo test -p jarvis-desktop` and `npm run test:ci` → PASS.
- [ ] **Step 5: Commit** `feat(desktop): mentor IPC`.

### Task 11: Mentor store and chat UI

**Files:**
- Create: `apps/desktop/src/app/core/state/mentor.store.ts` (+ `.spec.ts`), `apps/desktop/src/app/features/chat/mentor-bar/` (component, template, styles, spec), `apps/desktop/src/app/features/chat/mentor-start/` (dialog: project folder from workspace folders or "Add folder…", model)
- Modify: `apps/desktop/src/app/core/state/voice.store.ts` (route `conversation` transcripts to `MentorStore`), `apps/desktop/src/app/features/chat/chat.html|ts` ("Start a lesson" button, bar), the approval dialog (voice answers)
- Test: specs above, `voice.store.spec.ts`

**Interfaces:**
- Consumes: Task 10 API; `ChatStore.send(text, speak, attachments, …)`; `capture_screen`.
- Produces: `MentorStore.handlePhrase(transcript: VoiceTranscript)`; `MentorStore.status: Signal<MentorStatus>`.

- [ ] **Step 1: Write failing tests** `holds_phrases_until_the_reply_ends` (two phrases during an answer → one send with both, joined by a newline, after it finishes), `commands_never_reach_the_model` (Pause/End/Stop call the API; Look captures the screen and attaches it to the next phrase), `nudge_sends_a_files_saved_turn_only_when_idle`, `voice_yes_approves_a_medium_write_but_not_an_overwrite`, `bar_shows_listening_paused_typing_and_errors`, `start_dialog_lists_workspace_folders`, `continue_by_push_to_talk_resumes` (paused, push-to-talk «продовжуємо» → `mentor_resume`, not sent to the model).
- [ ] **Step 2: Run** `npm run test:ci` → FAIL.
- [ ] **Step 3: Implement.** Mentor turns always `speak: true` and `mentor: 'phrase' | 'filesSaved'`. Report busy on send start, clear after speech ends. Bar text "Mentor is listening" with Pause/Resume, "Look at the screen", "Stop typing", "End lesson". A wake-word phrase «починаємо урок» opens the start dialog (or resumes when the open conversation is a mentor one).
- [ ] **Step 4: Run** `npm run test:ci`, `npm run lint:angular`, `npm run lint:ox`, `npm run lint:styles`, `npm run format:check` → PASS.
- [ ] **Step 5: Commit** `feat(ui): mentor mode in chat`.

### Task 12: Documentation and live check

**Files:**
- Create: `docs/decisions/0016-mentor-mode.md`
- Modify: `ARCHITECTURE.md` (voice 4.3c diagram + new mentor section, IPC table), `SECURITY.md` (mentor writes, mic, cloud data), `CHANGELOG.md`, `ROADMAP.md`

- [ ] **Step 1:** Write the ADR (context, decision = approach B, consequences, limits: no echo cancellation, partial files during typing, cloud data).
- [ ] **Step 2:** Update the four docs.
- [ ] **Step 3:** Run the full local CI (fmt, clippy `-D warnings`, `cargo test --workspace --locked`, bindings diff, notices, frontend `format:check`, `lint:ox`, `lint:angular`, `lint:styles`, `test:ci`, `build`) → all pass.
- [ ] **Step 4:** Live check with a test profile and a scratch project in a workspace folder (Claude Code model, voice engines installed): start a lesson, two spoken phrases, a save that triggers a comment, "подивись на екран", "напиши сам" with voice approval, an edit during typing, "закінчуємо урок". Record timings.
- [ ] **Step 5:** Commit `docs: mentor mode` and push after CI is green.

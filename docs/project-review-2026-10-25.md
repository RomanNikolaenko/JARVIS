# Project review — 2026-10-25

Scope: local working tree, architecture and roadmap, CI/release configuration,
voice event transport and UI state, plugin state, and subprocess execution.
This is a focused engineering review, not an exhaustive security audit.
The working tree changed during the review (including the package version);
the findings below concern the inspected code, not a frozen release commit.
No production code was modified for this review.

## Findings

Follow-up: all six findings below have now been addressed in the working tree.
Voice events use one ordered queue (including individual capture starts), with
a bounded five-second frontend buffer for cross-event delivery delays. Snapshot
epochs protect microphone/plugin state; failed listening clears test intent;
subscriptions are disposed on failure/destruction, including late registration.
Subprocess pipe readers now run inside the same deadline/cancellation future as
the process, with no detached reader tasks. Regression coverage was added to the
existing suites. The original findings below are retained as review history.

Follow-up regression validation: 352 UI tests passed, plus 55 tests across
`jarvis-tools` and `jarvis-voice`. Coverage includes stale snapshots, reversed
voice event delivery, failed test intent, context changes while buffering,
partial/late listener cleanup, ordered native capture events, and inherited
subprocess pipes under both timeout and cancellation. The real-microphone test
remains intentionally ignored.

### 1. P1 — A stale status response can hide an open microphone

Location: `apps/desktop/src/app/core/state/voice.store.ts:116`.

`refresh()` unconditionally applies `status.state` after awaiting IPC. A newer
`onVoiceState` event received during that wait is overwritten. The global
microphone indicator derives directly from this state.

Reproduced with a controlled promise: begin refresh with an idle snapshot,
deliver a live `micOpen: true` event, then resolve the snapshot. The store
reports `micOpen: false`. The actual microphone is unaffected by the UI update.

Fix direction: revision/epoch protection for snapshot application, preferably
with a core state revision shared by snapshots and events.

### 2. P2 — Voice requests can be silently lost due to event ordering

Locations: `apps/desktop/src/app/core/state/voice.store.ts:173` and
`apps/desktop/src-tauri/src/lib.rs:290`.

The frontend drops transcripts whose capture ID has not yet appeared in a
state event. State and transcripts are forwarded by separate asynchronous
tasks; the state source is a watch channel, which retains only its latest
value. There is no cross-channel ordering or per-capture delivery guarantee.

Reproduced by delivering a valid transcript before the matching state event:
the later state does not recover the request. Unknown-ID rejection is a useful
safety measure, but ordinary requests need a reliable way to establish their
capture identity before recognition arrives.

Fix direction: an ordered capture-start/transcript protocol, or an explicit
capture acknowledgement with a bounded pending-result buffer. Preserve the
conversation checks; do not simply send unknown transcripts to the current chat.

### 3. P2 — Failed microphone tests retain their intent

Location: `apps/desktop/src/app/core/state/voice.store.ts:127`.

`listen(true)` sets `testCapture` and `captureContext` before starting the
microphone. A rejected start only updates the error through `run()`; the intent
remains until an unrelated new capture consumes it.

Reproduced by rejecting `startListening`, then delivering a wake-word capture
and transcript. The spoken request becomes `testTranscript` instead of a chat
request. This can happen when voice operation is restored or wake-word mode
is enabled after the failure.

Fix direction: bind test intent to a confirmed capture ID and clear failed
start intent, guarding against completion of an older overlapping request.

### 4. P2 — Initial plugin snapshot overwrites newer events

Location: `apps/desktop/src/app/core/state/plugins.store.ts:32`.

`start()` registers a listener and then unconditionally applies `listPlugins()`.
A plugin event arriving while the list is in flight is lost when the snapshot
resolves. A disabled or failed plugin can consequently appear enabled or running
until another event updates the list. This is a display/state problem; the
test does not demonstrate that the backend re-enables the plugin.

Reproduced with a pending initial list: deliver a disabled-plugin event, then
resolve an older enabled-plugin snapshot. The store ends with `enabled: true`.

Fix direction: event revision or snapshot epoch protection, including command
responses that can otherwise overwrite newer plugin events.

### 5. P2 — Voice/plugin subscriptions are not cleaned up after partial startup

Locations: `apps/desktop/src/app/core/state/voice.store.ts:79` and
`apps/desktop/src/app/core/state/plugins.store.ts:26`.

Both stores discard the returned unlisten functions. If a later startup step
fails, `started` becomes false, but successfully installed listeners remain.
A retry registers additional listeners. Voice store destruction clears only
the notice timer. This finding is based on control-flow inspection, not a new
runtime reproduction.

Fix direction: retain cleanup callbacks, release partial registrations on
failure and destruction, and release registrations that resolve after disposal.

### 6. P2 — Command timeout excludes output collection

Location: `crates/jarvis-tools/src/command.rs:140`.

The timeout/cancellation select covers `child.wait()`. Afterwards, output reader
tasks are awaited without that deadline or cancellation token. If the direct
child exits while a descendant holds inherited stdout/stderr handles, EOF may
not arrive and the configured command timeout no longer applies. Dropped reader
join handles also leave their tasks detached on timeout/cancellation paths.

The agent has an outer 15-minute tool timeout
(`crates/jarvis-agent/src/orchestrator.rs:42,823`), so this is not a claim that
the whole agent waits forever. It can nevertheless ignore a much shorter command
timeout and a stop request while waiting for output. This finding is based on
control-flow inspection; no subprocess reproduction was run in this review.

Fix direction: one deadline/cancellation scope for process and pipe collection,
with explicit reader task cleanup; process-tree containment is separate work.

## Validation

- Ran the UI suite with four additional temporary diagnostic cases.
- All 342 existing tests passed. All four new cases failed at the assertions
  described above, confirming findings 1–4 under controlled event timing.
- The test run reported 358 passed and 4 failed because the temporary files
  also copied 16 existing tests to reuse their fixtures.
- Removed the two temporary test files and temporary runner configuration.
- Did not rerun the full Rust workspace, desktop installers, native GUI,
  microphone, Linux checks, dependency advisory scans or real AI services.
- Findings 5–6 were inspected statically and should receive regression tests
  before changes are implemented.

## Architecture and delivery

The project has a substantial modular foundation: Rust domain crates separated
from the Tauri shell, generated IPC types, a local SQLite memory layer,
explicit tool policy/approval boundaries, and CI jobs for formatting, linting,
tests, dependency audits and installers. Existing tests cover many cancellation
and late-result scenarios, but voice/plugins still lack equivalent lifecycle
and event-ordering protections.

Open work already documented in ROADMAP.md includes native provider tool
calling, process sandboxing, memory encryption at rest, broader Linux live
verification, and native WebDriver coverage. Treat these as planned features
or accepted limitations, not newly discovered regressions.

Documentation drift: SECURITY.md still lists the RustSec CI scan as planned,
while `.github/workflows/ci.yml` already contains a `cargo audit` job. README.md
labels the project Phase 8, while ROADMAP.md names Phase 9 as the next iteration.

Recommended order: correct microphone state reporting; establish reliable voice
capture delivery and intent lifecycle; protect plugin snapshot/event ordering;
clean up subscriptions and subprocess readers; then resume native tool calling.

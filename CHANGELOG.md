# Changelog

All notable changes to JARVIS. Versions follow [Semantic Versioning](https://semver.org).

## [Unreleased]

### Fixed

- Anthropic requests no longer fail on current models: JARVIS sends API version
  `2026-10-21` without the sampling temperature those models reject, allows
  32 000 output tokens (thinking included), passes attached images, reads the
  context window from the Models API and reports refusals as such.
- Requests to the official OpenAI API use `max_completion_tokens` and leave out
  the temperature for reasoning models (`gpt-5*`, o-series), which reject it.
- Reached usage limits are reported as a limit (with the provider's message)
  instead of a generic failure, and are not retried.
- Model library changes are published only after the index is saved. Failed
  imports, projector changes and removals keep the committed state available
  for retry; failed file deletion restores its library entry when possible.
  Exact download retries preserve the model ID, projector and original date.
- Downloads cannot replace another model or an unrelated file sharing the same
  name. Existing bytes are reused only after verification; files appearing
  during a transfer are preserved, and conflicting registrations are rejected.
- Automatic summaries make bounded progress with long Unicode messages and
  oversized previous summaries in small context windows, while preserving the
  original messages and six most recent messages.
- A late send acknowledgement no longer navigates back to an earlier chat after
  the user opens another conversation, starts a new one or leaves the screen.
- Chat requests recheck current cloud, image and saved-memory permissions after
  discovery and immediately before dispatch, using the connection actually held
  by the request. Embedding batches and automatic conversation summaries also
  honor revoked permissions. Turns refused during preparation retain captures
  for an explicit retry.
- Sending while another conversation is opening no longer targets the previous
  conversation or creates an unintended new one. Send, Enter, pinned-note saves
  and project changes wait until loading completes, preserving unsent drafts.
- Paused agent tasks resume only with their recorded provider/model. Missing
  identities, unavailable original models, unfinished actions and disabled
  agents are refused without rerouting the journal or changing the saved task.
  The task panel explains the refusal and can refresh the original model list.
- Agent authorization is checked again after asynchronous model discovery and
  before each new model request using the provider actually held by the task.
  Turning agents off also disables built-in and plugin tools at the execution
  policy check, including actions that were waiting for approval.
- Saving model routes preserves edits made while the earlier save was pending.
  Save/Discard and conflicting provider operations stay blocked until the
  operation completes; failed saves keep the draft for an explicit retry.
- Provider settings, API-key writes and catalog refreshes share one UI guard
  across page instances. Keys are captured before awaiting a settings save,
  and removing the edited provider closes its obsolete editor.
- Explicit model selections survive catalog changes. If a chosen model
  disappears, chat and agent creation show it as unavailable and preserve the
  draft instead of silently switching to Automatic. Returning models restore
  the selection; Automatic requires an explicit choice. Pending chat creation
  retains the original provider/model IDs, and blocked sends keep attachments.
  Routing selectors also label missing or category-incompatible saved choices
  as unavailable without rewriting their references.
- Provider connection tests have a 30-second UI deadline and can be dismissed
  while waiting. Late replies cannot affect a reopened editor or a newer test;
  editing connection details clears the previous result. Missing provider names
  and invalid API keys now have visible validation messages.
- Connection tests keep local and LAN servers usable when the OS credential
  vault is unavailable, matching provider registration. Cloud credential lookup
  failures stop before any provider request; explicit test keys use the same
  byte-length and control-character checks as saved keys.
- Invalid local-model replacements (missing model files/projectors, incompatible
  embedding pooling or a changed engine) no longer stop a healthy chat or
  embedding server before reporting the error.
- Model availability and routing update on every screen after the core registry
  is ready, including autostart and model changes that finish between polls.
  Catalog refreshes precede route resolution; missed subscriptions are retried
  after connection recovery and stale replies cannot replace newer selections.
- Execution errors, cancellation, shutdown and timeouts during a started
  non-idempotent tool now leave an `unknown` action result and stop the task
  without another model turn. User cancellations remain cancelled; other
  uncertain actions fail instead of becoming resumable. Their journals survive reopening and
  bulk clearing, with an operation ID and manual-review warning.
  After checking the effects, users can explicitly delete one reviewed journal
  without removing its audit entries or other unresolved tasks.
- The local model library explains when Start is unavailable because the
  llama.cpp engine is missing, with a link to the engine installation controls.
- New-file writes flush Tokio's buffered file operation before reporting
  success, so readers cannot observe an incomplete write after a successful
  tool result.
- Agent approvals are checked again immediately before execution using current
  permissions, workspace folders, program/network access and tool limits.
  Removed or replaced plugin tools and changed prepared actions are refused;
  approving file creation cannot authorize a later overwrite or a different
  workspace target. Refusals remain in the journal and audit log.
- The task journal expands failed, denied and unknown action output so the
  reason is visible immediately. Unknown results show their operation ID and
  a manual-review warning; running actions use a readable status label.
  Clearing finished tasks preserves unresolved journals and their selection.
- Agent tools now persist an authorized execution intent before running, using
  the task ID and journal index as the operation identity. Recovery marks an
  unfinished intent as an unknown result and stops the task; users must inspect
  its effects before starting new work. SQLite now uses FULL synchronization
  so acknowledged journal commits survive power loss on supported storage.
- Invalid or unavailable answer verification produces an explicit `unverified`
  terminal status and UI warning, preserving the candidate answer. Corrected
  answers are checked again; a second rejection fails the task. Schema migration
  4 preserves existing tasks and audit history while adding the new status.
- Journal or audit write failures now stop an agent before another action.
  A failed approval checkpoint prevents execution even if the user approved.
  Unsaved terminal snapshots remain visible through task reads and listings;
  recovery cannot resume their stale database rows in the same session.
  Clearing finished tasks retains these snapshots if storage still rejects
  writes. This in-memory fallback does not survive application exit.
- Failed or cancelled initial task saves no longer leak agent concurrency
  slots. Lifecycle transitions serialize resume/cancel persistence, concurrent
  resumes cannot launch duplicate runs, and shutdown rejects new launches.
  A reservation guard also releases slots when a run unwinds.
- The agent's wall-clock deadline now interrupts planning, text/native model
  generation, verification, pauses, pending approvals and running tools.
  Previously it was checked only between steps. Expired approvals are withdrawn,
  late approvals cannot start a tool, and interrupted actions retain their
  journal/audit record with a warning that they may have partly happened.

### Added

- ChatGPT and Claude subscriptions as providers: "ChatGPT subscription (Codex
  CLI)" and "Claude subscription (Claude Code)" run the vendor's installed CLI,
  signed in by the user with `codex login` / `claude auth login`, without an
  API key. The CLI's own tools, plugins, MCP servers and hooks are switched
  off, it runs in an empty temporary folder and is stopped with the request.
  Both count as cloud providers. See ADR 0015.

- Running local chat and embedding models have a Restart action for applying
  saved inference settings. It refuses stale process selections and waits for
  pending operations or unsaved settings to be resolved.

- Core-level native tool regression tests now exercise both Ollama and
  OpenAI-compatible HTTP adapters with persisted settings and real scoped file
  tools: approval/denial, HTTP and approval cancellation, late approval,
  invalid calls, verifier failure and completed-journal recovery. These use
  scripted local servers; live model and desktop end-to-end validation remain
  separate checks.
- The embedded llama.cpp runtime can opt individual chat models into native
  agent tools. Select models under Runtime → Inference settings, save and
  restart the chat server. Selected models launch with `--jinja` and advertise
  tool calling; other chat models retain the text protocol, and embeddings
  never advertise tools. The runtime panel shows the active mode.
- A local-only `native-agent-probe` diagnostic exercises the real agent and
  HTTP provider pipeline with a temporary read-only fixture, deadlines, journal
  and audit checks. Scripted HTTP tests cover Ollama/OpenAI-compatible protocols
  and reject answers that skip the tool. Usage is documented in
  `docs/NATIVE-TOOLS-VALIDATION.md`.
- The provider editor accepts exact model IDs with native tool support for
  OpenAI-compatible endpoints. Overrides are validated, persisted and applied
  after saving; other models retain conservative capability detection. Old
  configurations default to an empty override list.
- Native non-streaming tool transport for Ollama and OpenAI-compatible
  providers, including tool schemas, calls and result history. Responses are
  size-bounded and malformed or truncated calls are rejected.
- Agents select native tool calling when both the model and provider adapter
  support it, retaining the text protocol otherwise. Native actions pass
  through existing policy, approvals and audit; journal recovery rebuilds
  call/result pairs without replaying actions. Plugin names use request-local
  aliases, and multiple or unadvertised calls are rejected before execution.

### Changed

- Centralized observation of an owned process's root exit in the platform
  layer. MCP servers retain ownership of descendants until cleanup, including
  when the root process exits first.

### Tests

- Added process-tree regression tests for live-root disposal and root exit
  with surviving descendants, plus subscription deadline/disposal tests.

## [0.1.1] — 2026-10-25

### Changed

- The Windows installer and uninstaller show the JARVIS icon instead of the
  NSIS default, and the installer moves on to the finish page by itself
  once the files are installed.

## [0.1.0] — 2026-10-25

The first version: a local-first desktop assistant for Windows 10/11 and
Linux (X11 and Wayland).

### Added

- **Foundation**: Rust workspace, Tauri 2 shell, Angular 22 UI with a dark
  HUD design; typed IPC with generated TypeScript types and deny-by-default
  permissions; health checks, system event log, rotating logs; CI on
  Windows and Linux.
- **AI providers**: Ollama and OpenAI-compatible servers (auto-detected),
  OpenAI, Anthropic and Gemini with API keys in the OS keychain; model
  routing by task; streamed chat with stop. Checked live against Ollama
  0.40.11 and the llama.cpp server (chat, Stop, embeddings for memory);
  token usage is requested from OpenAI-compatible servers.
- **Embedded runtime**: official llama.cpp builds installed after approval,
  verified GGUF downloads, a supervised server with memory checks, an
  embedding server for semantic search.
- **Memory**: conversations, facts, projects and an action history in a
  local SQLite database; search by words and meaning; export and import;
  context budgets with summaries.
- **Agents and tools**: multi-step tasks with file, program, system, web
  and git tools behind a Security Policy Engine, approvals and a
  tamper-evident audit log.
- **Voice and vision**: speech recognition (whisper.cpp) and speech (Piper)
  on the computer, wake word «Джарвіс», a microphone indicator; screenshots,
  windows, image files and camera photos for vision models.
- **Plugins and MCP**: plugins are MCP servers (protocol 2026-07-28 and the
  older handshake) reviewed and fingerprinted before every start; their
  tools go through the same policy; example plugin `text-tools`.
- **Window**: opens maximized; a restored window is never larger than the
  work area of its monitor (the screen without the taskbar).
- **Release**: installers (NSIS, MSI, deb, rpm, AppImage) for x64 and
  ARM64, a draft-release workflow with checksums and a "Which file do I
  need?" table; every installer except the rpm is installed, smoke tested
  and uninstalled on clean machines in CI. The Windows ARM64 installer
  refuses other processors and names the right file.
- **Updates**: signed automatic updates from GitHub Releases; an update
  button in the top bar shows the running version, offers a new one,
  downloads, verifies and installs it on one click with progress, and
  offers a retry after a failed download.
- **Developer tools**: `provider-probe` (real model servers through
  JARVIS's provider code) and `mcp-probe` (MCP servers) examples.
- **License**: MIT OR Apache-2.0; the installers include the licenses of
  all shipped third-party crates and packages (`THIRD-PARTY-NOTICES.txt`).

### Not included

- Cloud sign-in or sync: all data stays on the computer (ADR 0013).
- Authenticode-signed installers (need a certificate).

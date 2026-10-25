# JARVIS — Architecture

This document describes the architecture **as implemented** and marks clearly
what is planned. Decisions with trade-offs are recorded as ADRs in
[`docs/decisions/`](docs/decisions/).

## 1. Goals and principles

JARVIS is a local-first, privacy-first personal AI assistant for Windows and
Linux. Guiding principles: Local First, Privacy First, Security by Design,
modular monolith with clean boundaries, dependency inversion, provider-agnostic
AI, cross-platform compatibility, testability and extensibility.

We deliberately start with a **modular monolith** (one process, several Rust
crates with explicit interfaces) instead of services. See
[ADR 0001](docs/decisions/0001-modular-monolith.md).

## 2. Layers

```
┌───────────────────────────────────────────────────────────────┐
│ Presentation — Angular 22 (apps/desktop/src)                  │
│ standalone components, signals, zoneless, lazy routes         │
│ no direct access to files, secrets, OS APIs or databases      │
└──────────────▲─────────────────────────────┬──────────────────┘
               │ events / channels           │ typed IPC commands
┌──────────────┴─────────────────────────────▼──────────────────┐
│ Application — Tauri 2 shell (apps/desktop/src-tauri)          │
│ lifecycle, paths, logging, IPC commands, ACL, event forwarding│
└──────────────▲─────────────────────────────┬──────────────────┘
               │ Rust API (no Tauri types)   │
┌──────────────┴─────────────────────────────▼──────────────────┐
│ Domain — Rust core crates (crates/*)                          │
│ jarvis-core  → composition root: config, health, chat, router │
│ jarvis-ai    → AiProvider trait, Ollama/OpenAI-compatible     │
│ jarvis-runtime → supervised llama.cpp server, GGUF, downloads │
│ jarvis-voice → audio, VAD, whisper.cpp, Piper, wake word      │
│ jarvis-vision → screen/window capture, images for models      │
│ jarvis-events→ event bus + system event log                   │
│ jarvis-platform → OS adapters, hardware, system monitor       │
└───────────────────────────────────────────────────────────────┘
```

Dependency rule: arrows point inwards. Domain crates never depend on Tauri or
Angular; `jarvis-core` depends on the other domain crates, and the Tauri shell
depends only on the public API of the domain crates.

## 3. Repository layout

```
Cargo.toml                    Rust workspace (resolver 3, edition 2024)
crates/
  jarvis-events/              EventBus, SystemEvent, bounded history
  jarvis-platform/            PlatformAdapter trait, Windows/Linux adapters,
                              PlatformInfo, SystemMonitor, HardwareInfo (CPU
                              features, RAM, GPUs, Vulkan), process helpers
  jarvis-runtime/             RuntimeManager: llama.cpp installer, GGUF library,
                              verified downloads, supervised llama-server
  jarvis-storage/             SQLite database (bundled, FTS5), versioned
                              migrations with checksums and backups
  jarvis-memory/              MemoryEngine (conversations, facts, projects,
                              episodes, FTS + vector search, export/import),
                              Context Manager (token budgets, compaction)
  jarvis-security/            Security Policy Engine (capabilities, risk, scoped
                              file access, program allowlist, SSRF guard),
                              ApprovalBroker, hash-chained AuditLog
  jarvis-tools/               Tool trait (prepare → policy → run), registry,
                              file, search, process, system, web and git tools
  jarvis-agent/               Task manager (states, limits, persisted journal),
                              orchestrator (planner, executor, verifier,
                              recovery), text protocol for the model
  jarvis-voice/               VoiceEngine: audio I/O (cpal), VAD (earshot),
                              managed whisper-server and Piper processes,
                              wake word, sentence splitter, pinned packages
  jarvis-vision/              Screen/window capture (xcap on Windows, x11rb on
                              X11, xdg-desktop-portal on Wayland), image
                              decoding, downscaling and previews
  jarvis-ai/                  AiProvider trait, ProviderRegistry, ProviderFactory,
                              Ollama + OpenAI-compatible HTTP providers,
                              EchoProvider, provider config + URL privacy class
  jarvis-core/                JarvisCore, Settings/ConfigStore, health checks,
                              ChatService, model catalog + router, memory
                              integration (turns, embeddings, action history),
                              agents (policy from settings, model adapter,
                              privacy rules), error codes
apps/desktop/
  src/                        Angular UI
    app/core/ipc/             TauriBridge (only user of @tauri-apps/api), JarvisApi
    app/core/state/           signal stores (system, events, settings, models,
                              runtime, chat, memory, agents, voice, vision)
    app/core/errors/          global error handler → core log
    app/features/             dashboard, chat (+ conversation list), models,
                              runtime, agents (tasks, tools & permissions, audit
                              log), memory (facts, projects, history, search,
                              data), events, settings and Settings → Voice (lazy)
    app/shared/               HUD panel, AI core, icons, status badge, Markdown,
                              approval dialog, formatters
  src-tauri/                  Tauri 2 shell (crate jarvis-desktop)
    capabilities/             IPC permissions per window
    build.rs                  app command manifest, Windows manifest
packages/shared-types/        TypeScript IPC types generated from Rust (ts-rs)
docs/decisions/               Architecture Decision Records
.github/workflows/ci.yml      Windows + Linux CI
```

Crates that exist only on the roadmap (`jarvis-sync`) are **not created
yet**; they
are added when their phase starts, so the tree never contains empty modules.

## 4. Components (implemented)

### 4.1 `jarvis-core` — composition root

`JarvisCore::initialize(CoreOptions)` creates directories, loads settings,
collects platform info, registers AI providers and builds the `ChatService`.
Its public API is UI-agnostic:

| Method | Purpose |
| --- | --- |
| `info()` | name, version, build profile, OS, paths |
| `health()` | concurrent component probes with timeouts → `HealthReport` |
| `settings()` / `update_settings()` | validated, persisted settings |
| `system_metrics()` | CPU/RAM sample on a blocking thread |
| `list_models(refresh)` | models per provider from the cached catalog; per-provider errors |
| `resolve_routes()` | which model each task category currently uses |
| `test_provider(config)` | connect to an unsaved provider config and list its models |
| `chat()` | `ChatService`: start / cancel streamed chats |
| `record_ui_error()` | truncated, rate-limited UI error reports |
| `shutdown()` | cancels running work |

**Errors.** `CoreError` maps to a stable `ErrorCode` (`validation`,
`notFound`, `permissionDenied`, `unsupported`, `timeout`, `cancelled`,
`unavailable`, `limitExceeded`, `config`, `io`, `internal`). The UI branches on
codes only. `ErrorPayload { code, message }` is the IPC error format.

**Settings.** One TOML file (`config.toml`) in the OS config directory.
Missing fields get defaults; values are validated (ranges, cross-field rules);
writes are atomic (temp file + rename). An unreadable or invalid file is never
overwritten silently: JARVIS runs on defaults, health reports `degraded`, and
the file is backed up before the next save. See
[ADR 0004](docs/decisions/0004-settings-storage.md).

**Health.** Probes: configuration, local storage, event bus, platform layer,
AI providers. Probes time out after 3 s (AI: 6 s, 5 s per provider, because a
refused local connection alone takes ~2 s on Windows). The AI component is
`healthy` if at least one *real* provider answers (unreachable optional
providers are listed in the message); with only the diagnostic provider it is
honestly `degraded`.

### 4.2 `jarvis-ai` — provider abstraction

```rust
#[async_trait]
pub trait AiProvider: Send + Sync + Debug {
    fn descriptor(&self) -> &ProviderDescriptor;          // id, name, kind, locality
    async fn health_check(&self) -> Result<(), AiError>;
    async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError>;
    async fn capabilities(&self, model: &str) -> Result<ModelCapabilities, AiError>;
    async fn stream_chat(&self, req: ChatRequest) -> Result<ChatStream, AiError>;
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, AiError>;          // default: collect stream
    async fn embeddings(&self, req: EmbeddingRequest) -> Result<EmbeddingResponse, AiError>; // default: Unsupported
}
```

- **Capability detection:** `ModelCapabilities { chat, streaming, embeddings,
  toolCalling, vision }`. Optional operations default to `Unsupported`.
- **Privacy metadata:** every provider declares `DataLocality`
  (`onDevice`, `localNetwork`, `thirdPartyCloud`).
- **Cancellation:** dropping a `ChatStream` must stop the backend work; the
  core adds `CancellationToken`s on top. `cancel_request` therefore lives in
  `ChatService::cancel(request_id)` rather than in every provider.
- **Retries:** `AiError::is_transient()` marks retryable failures for
  idempotent calls only; chat requests are never retried automatically.
- **Tool calling** is represented in the capability model; its API arrives with
  the agent system (Phase 5).

Implementations ([ADR 0006](docs/decisions/0006-http-ai-providers.md)):

| Provider | Protocol | Capabilities |
| --- | --- | --- |
| `OllamaProvider` | `/api/chat` (NDJSON stream), `/api/tags`, `/api/show`, `/api/embed` | reported by the server (`completion`, `vision`, `tools`, `embedding`), context length from `model_info` |
| `OpenAiCompatibleProvider` | `/chat/completions` (SSE), `/models`, `/embeddings` | inferred conservatively from the model id (chat or embeddings; never vision/tools) |
| `EchoProvider` | none | diagnostic only: streams the user's message back |

Configured providers (`ai.providers` in settings) are turned into providers by
a `ProviderFactory` (injected; tests use fakes). Their `DataLocality` is
derived from the URL: loopback → `onDevice`; private/link-local/CGNAT,
single-label hosts and `.local`/`.lan`/`.home.arpa`/`.internal` →
`localNetwork`; anything else → `thirdPartyCloud`. The shared HTTP client has
no proxy, a 5 s connect timeout and retries idempotent requests once after
timeouts or 502–504. Streaming decoders handle lines and UTF-8 sequences split
across network chunks. See also [ADR 0003](docs/decisions/0003-ai-provider-abstraction.md).

### 4.2a Model catalog and router (`jarvis-core::catalog`, `::routing`)

- **Catalog:** cached snapshot of every provider's models (30 s; 5 s if a
  provider failed), refreshed on demand, invalidated when providers change.
  Concurrent callers share one refresh.
- **Router:** task categories `general`, `reasoning`, `coding`, `vision`,
  `embeddings`, `toolCalling` with required capabilities. Explicit choices in
  `ai.routing` win (`reasoning`/`coding` inherit `general`); otherwise the
  router ranks real before diagnostic, on-device before LAN before cloud, then
  provider and model order. Cloud models are candidates only when allowed. See
  [ADR 0007](docs/decisions/0007-model-routing.md).
- **Reload:** saving settings with changed providers rebuilds the registry
  atomically; running chats keep their provider handle.

### 4.2b Embedded runtime (`jarvis-runtime`)

See [ADR 0008](docs/decisions/0008-embedded-llama-runtime.md).

| Module | Responsibility |
| --- | --- |
| `installer` | find official llama.cpp builds (GitHub, tags `bNNNNN`), re-verify an approved release, extract `.zip`/`.tar.gz` safely, locate and probe `llama-server` |
| `gguf` | bounded GGUF metadata reader (incl. the pooling type of embedding models); memory estimate (weights + KV cache + overhead) |
| `library` | model index (`models/library.json`); import in place; deletes only files inside the models folder (canonical paths) |
| `catalog`, `download` | curated pinned models; Hugging Face resolution to commit + LFS SHA-256; resumable, verified downloads with progress and cancellation |
| `process` | command line (chat or `--embedding` with pooling and batch = context), spawn without console, log ring buffer, `/health` readiness, API-key check |
| `RuntimeManager` | one `Slot` per `ServerRole` (`chat`, `embeddings`): state machine `stopped → starting → running → stopping/failed`, supervision task, provider publication via `watch`, pid file and orphan cleanup; shutdown stops both |

The core owns the `RuntimeManager`, adds the providers of the running servers
to the registry (`jarvis-runtime` for chat, `jarvis-runtime-embeddings` for
embeddings; before configured servers, so automatic routing and the choice of
embedding model prefer them) and rebuilds the registry, then wakes the memory
indexer, whenever a server starts or stops. Health reports one `runtime`
component covering both servers; autostart happens only for
`runtime.autostartEmbeddingModel` / `runtime.autostartModel` (embedding model
first). Data lives in the local data directory: `runtime/` (engine, install
record, pid files) and `models/`.

### 4.3 Chat orchestration (`jarvis-core::chat`)

`ChatService::start(request, sink)`:

1. validates input (≤ 200 messages, ≤ 32 000 chars each, ≤ 256 000 total,
   at least one user message, temperature 0–2, …);
2. resolves provider and model: an explicit `providerId` + `model` pair is
   used as is; otherwise the catalog and the router (`general` category)
   decide;
3. **privacy policy:** `thirdPartyCloud` providers are rejected unless
   `privacy.allowCloudProviders` is enabled — for explicit pairs before any
   network request (blocked attempts are logged as security events);
4. checks the model's `chat` capability;
5. enforces ≤ 4 concurrent requests;
6. spawns a task that streams `ChatStreamEvent`s to the sink with an overall
   timeout (`ai.requestTimeoutSecs`) and an idle timeout between chunks
   (`ai.idleTimeoutSecs`), honouring cancellation.

Every accepted request ends with exactly one `finished` (reason `stop`,
`length` or `cancelled`) or `failed` event, unless the receiver disappears.

### 4.3a Memory (`jarvis-storage`, `jarvis-memory`, `jarvis-core::memory`)

See [ADR 0009](docs/decisions/0009-local-memory.md).

| Layer | Stored as | Used for |
| --- | --- | --- |
| Working | pinned note per conversation + recent turns | every request of the conversation |
| Conversation | `conversations`, `messages`, `summaries` | history, recall from other chats, compaction |
| Semantic | `facts` (global or per project, pinned or not) | pinned facts always, others when relevant |
| Project | `projects` (description, instructions) | injected into every chat of the project |
| Episodic | `episodes` (actions from the event bus, no message text) | Memory screen, search |

A conversation turn (`JarvisCore::send_message`):

1. validates the message; refuses saved conversations while saving is off;
   one turn at a time per conversation;
2. resolves provider and model (same rules as 4.3) and the context window:
   the running llama.cpp context for the embedded runtime, at most 4 096 for
   Ollama, otherwise the reported window, capped by
   `memory.contextLimitTokens`;
3. retrieves pinned and relevant facts and excerpts of other conversations
   of the same project scope — only for on-device/LAN models unless
   `privacy.shareMemoryWithCloud` is on — with FTS5 and, when an embedding
   model is available, vector similarity (reciprocal rank fusion); vector
   hits must reach the embedding model's floor and lie within 0.10 of the
   best recallable hit (`SimilarityCuts`; measured for BGE-M3: recall 0.55,
   search 0.52); text hits must share half of the query's terms and more
   than half as many as the best text hit, and one that shares a single
   word of the question falls when the semantic side matched other facts
   or excerpts instead (`TextQuery`);
4. the Context Manager assembles system prompt, project, note, memory,
   summary and recent messages within the budget and reports what it used
   (`ContextReport`, sent with the `started` event);
5. saves the user message, streams the reply through `ChatService`, saves
   the reply (or the partial reply of a stopped request) when it ends;
6. if older messages no longer fit, the same model writes a summary of the
   oldest uncovered ones in the background.

Background tasks: the indexer embeds new facts and messages (batches of 32,
on change and every 60 s), the recorder writes actions to the history every
5 s and on shutdown, the pruner removes actions older than the retention.

### 4.3b Tools and agents (`jarvis-security`, `jarvis-tools`, `jarvis-agent`, `jarvis-core::agents`)

See [ADR 0010](docs/decisions/0010-tools-and-agents.md).

```
model reply ──parse──▶ tool call ──prepare (no side effects)──▶ ActionRequest
                                                                  │
          SecurityPolicy::evaluate ◀──────────────────────────────┘
            ├─ deny ─────────────▶ audit: policyDenied ─▶ result "refused" to the model
            ├─ approval ─▶ ApprovalBroker ─▶ UI dialog ─▶ approved / denied / timed out
            └─ allow ────────────▶ audit: allowed
                                     ▼
                              Tool::run (re-checks scope / DNS) ─▶ audit: succeeded / failed
                                     ▼
                       <tool_result> (untrusted data) ─▶ next model turn
```

| Tool | Risk | Notes |
| --- | --- | --- |
| `list_directory`, `read_file`, `search_files`, `system_info`, `list_processes`, `git` (status/log/diff/show/branch) | low | on by default; files only inside workspace folders; git without hooks, pager, external diff or credential helpers |
| `write_file` (new file), `edit_file`, `copy_path`, `move_path`, `fetch_url`, `open_url` | medium | `fetch_url`: public http(s) only, DNS re-checked per redirect, text only |
| `write_file` (overwrite), `delete_path` (file or empty folder), `run_command`, `launch_program`, `stop_program` | high | always need approval; programs from the allowlist, no shell, time limit; only programs JARVIS started can be stopped |

**Settings** (`tools`, `agents`, `privacy.allowCloudAgents`) are read
before every step, so a change applies to running tasks. Protected paths:
JARVIS's config, data and log folders.

**Task lifecycle:** `pending → running ⇄ waitingForApproval`, `running ⇄
paused`, then `completed | failed | cancelled`. The journal (`TaskStep`:
plan, action, answer, verification, note — with thought, input, decision,
outcome, output, duration) is saved to `agent_tasks` after every step and
pushed to the UI (`jarvis://agent-task`). Pending approvals are pushed as a
list (`jarvis://approvals`); a new request also asks the OS to draw
attention to the window.

**Model:** `AgentModel` is implemented in the core: explicit choice →
`toolCalling` route → general route, the chat privacy check, plus
`allowCloudAgents` for cloud models; temperature 0.2, at most 1 024 output
tokens per turn, the request timeout from settings; the conversation sent
to the model is trimmed to its context window (oldest steps first).

### 4.3c Voice and vision (`jarvis-voice`, `jarvis-vision`, `jarvis-core::voice`, `::vision`)

See [ADR 0011](docs/decisions/0011-voice-and-vision.md).

```
microphone ─cpal─▶ 16 kHz mono ─VAD─▶ phrase ─whisper-server─▶ text
   push-to-talk ─────────────────────────────▶ chat turn (speak: true)
   wake word: «Джарвіс …» ───────────────────▶ chat turn, or wait for the command
reply deltas ─sentences─▶ Piper (stdin JSON) ─▶ WAV ─▶ speakers   (barge-in cuts it)

capture button ─▶ ScreenCapture (xcap │ x11rb │ portal) ─▶ JPEG ≤ 1568 px + preview
   ─▶ CaptureStore (memory, 30 min, used once) ─▶ ChatMessage.images ─▶ vision model
```

- Engines run as child processes from `<data>/voice`: `whisper-server` on
  loopback with a random port and a secret path prefix, Piper with one JSON
  request per sentence. Every package is pinned with its SHA-256; program
  and library hashes are checked before every start.
- The microphone opens only for push-to-talk or in wake word mode
  (`voice.wakeWord`); `VoiceState.micOpen` drives the indicator shown on
  every screen (`jarvis://voice-state`), and opening/closing is logged.
- A message with images goes to the `vision` route; cloud models get
  images only with `privacy.shareCapturesWithCloud`. The embedded runtime
  passes a model's projector with `--mmproj`.

### 4.4 `jarvis-events` — event bus

Typed `SystemEvent { id, timestampMs, level, category, message }`, published
through a `tokio::sync::broadcast` channel and kept in a bounded in-memory
history (500). Events are user-facing; diagnostic logs go through `tracing`.
Every event is mirrored to `tracing` as well. Slow subscribers get a `Lagged`
notification instead of blocking publishers.

### 4.5 `jarvis-platform` — cross-platform layer

- `PlatformAdapter` trait (`os_family`, `display_server`); the **only** module
  with target `cfg`s is `adapters.rs` (Windows, Linux, explicit fallback).
- Linux display server detection (`XDG_SESSION_TYPE`, `WAYLAND_DISPLAY`,
  `DISPLAY`) is a pure function, tested on every OS.
- `PlatformInfo` (static) and `SystemMonitor` (CPU/RAM/process usage via
  `sysinfo`). Calls are blocking and run on `spawn_blocking`. The monitor is
  created lazily and warmed up in the background, because the first CPU
  counter setup takes ~1.5 s on Windows and must not delay startup.
- Unsupported capabilities must be reported as such, never faked.

### 4.6 Application layer (`apps/desktop/src-tauri`)

- **Startup:** resolve paths (config: roaming config dir; data: *local* data
  dir — large files must not roam; logs: OS log dir) → init logging → init core
  → apply configured log level → start event forwarder → manage `AppState`.
- **IPC commands** are thin adapters (`commands.rs`). Async commands return
  `Result<T, ErrorPayload>`.
- **Streaming:** chat uses a `tauri::ipc::Channel<ChatStreamEvent>` per request
  (ordered, request-scoped). System events are pushed with
  `emit_to("main", "jarvis://system-event")`.
- **Shutdown:** on `RunEvent::Exit` the core cancels running chats and the log
  writer is flushed.
- **Logging:** `tracing` to stderr and to daily-rotated files
  (`jarvis.YYYY-MM-DD.log`, 14 files kept). Own crates log at the configured
  level, dependencies at `warn`; `RUST_LOG` overrides. Panics are logged.

### 4.7 IPC contract

| Command | Arguments | Result |
| --- | --- | --- |
| `get_app_info` | – | `AppInfo` |
| `get_health` | – | `HealthReport` |
| `get_platform_info` | – | `PlatformInfo` |
| `get_system_metrics` | – | `SystemMetrics` |
| `get_settings` | – | `Settings` |
| `update_settings` | `settings: Settings` | `Settings` |
| `get_recent_events` | `limit?: number` (1–500) | `SystemEvent[]` |
| `list_ai_models` | `refresh?: boolean` | `ProviderModels[]` |
| `get_model_routes` | – | `RouteResolution[]` |
| `test_ai_provider` | `config: ProviderConfig` | `ProviderTestResult` |
| `send_message` | `request: ChatTurnRequest`, `onEvent: Channel<ChatStreamEvent>` | request id |
| `cancel_chat` | `requestId: string` | `boolean` |
| `report_ui_error` | `report: UiErrorReport` | – |
| `get_runtime_status` | – | `RuntimeStatus` |
| `get_hardware_info` | – | `HardwareInfo` |
| `list_curated_models` | – | `CuratedModel[]` |
| `check_runtime_release` | `backend` | `RuntimeRelease` (GitHub lookup) |
| `install_runtime` | `release: RuntimeRelease` (approved) | download id |
| `select_runtime_binary` | – (native picker in Rust) | `RuntimeBinary \| null` |
| `import_model` | – (native picker in Rust) | `LocalModel \| null` |
| `download_curated_model` / `download_model` | `id` / `request: ModelDownloadRequest` | download id |
| `cancel_download` | `id` | `boolean` |
| `remove_model` | `id`, `deleteFile` | – |
| `start_local_model` / `stop_local_model` | `modelId` / – | – |
| `list_conversations` / `get_conversation` | `query?`, `limit?` / `id` | `Conversation[]` / `ConversationDetail` |
| `create_conversation` / `update_conversation` / `delete_conversation` | `projectId?`, `temporary` / `id`, `patch` / `id` | `Conversation` / `Conversation` / – |
| `list_facts` / `save_fact` / `delete_fact` | – / `input: FactInput` / `id` | `Fact[]` / `Fact` / – |
| `list_projects` / `save_project` / `delete_project` | – / `input: ProjectInput` / `id` | `Project[]` / `Project` / – |
| `list_episodes` / `clear_episodes` | `limit?`, `before?: EpisodeCursor` / – | `Episode[]` / count |
| `search_memory` | `query: SearchQuery` | `SearchHit[]` |
| `get_memory_status` | – | `MemoryStatus` |
| `export_memory` / `import_memory` | – (native dialogs in Rust) | `TransferReport \| null` |
| `clear_memory` | `confirmation` (fixed phrase) | – |
| `get_agent_status` / `list_tools` | – | `AgentStatus` / `ToolInfo[]` |
| `start_agent_task` | `request: AgentStartRequest` | `AgentTask` |
| `list_agent_tasks` / `get_agent_task` | `limit?` / `id` | `TaskSummary[]` / `AgentTask` |
| `pause_agent_task` / `resume_agent_task` / `cancel_agent_task` | `id` | `boolean` / – / `boolean` |
| `clear_finished_agent_tasks` | – | count |
| `list_pending_approvals` / `resolve_approval` | – / `id`, `approved` | `PendingApproval[]` / `boolean` |
| `get_audit_log` / `verify_audit_log` | `limit?`, `beforeId?: UUID` / – | `AuditEntry[]` / `AuditVerification` |
| `add_workspace_folder` | – (native folder dialog in Rust) | `Settings \| null` |

Audit pages follow reverse append order, matching the hash chain rather than the
wall clock. `beforeId` is the last displayed entry's UUID, resolved through the
existing unique index to its SQLite sequence. Equal timestamps and clock rollback
cannot skip records. A missing cursor entry returns an error instead of silently
reporting the end of the log. No schema or hash format migration is required.

The audit view coalesces concurrent page reads and lets a first-page refresh
supersede an older-page request. Only the current read can update entries, loading
state or errors. Reads have a 30-second frontend deadline and settle when the
store is destroyed; this does not cancel native commands. Audit read errors stay
visible independently of task operations, and previously loaded rows survive a
failed refresh. Reaching the last page prevents further older-page requests.

Integrity checks use a separate coalesced request, loading indicator and error,
with the same frontend deadline and destruction handling. They do not block task
operations or page reads. Starting a recheck clears the previous verdict; failures
cannot leave a stale success visible. Completed verdicts are labelled as the last
check, since the journal may change afterward. Late native responses after a
timeout cannot replace the result of a retry.

Events: `jarvis://system-event` → `SystemEvent`; `jarvis://agent-task` →
`AgentTask`; `jarvis://approvals` → `PendingApproval[]`.

Types are defined once in Rust and generated into
`packages/shared-types/src/generated/*.ts` by `ts-rs` during `cargo test`; CI
fails if they drift. Every command is declared in `build.rs` (app manifest) and
must be granted by a capability; anything not granted is denied. See
[ADR 0002](docs/decisions/0002-ipc-contract.md).

### 4.8 Presentation layer

- Zoneless Angular with signals and `OnPush` everywhere; features are lazy
  routes.
- `TauriBridge` is the only file importing `@tauri-apps/api`; `JarvisApi`
  exposes typed commands. When the UI runs outside Tauri (e.g. `ng serve` in a
  browser) every call fails with `unavailable` and the UI shows *Core offline*
  instead of fake data.
- Stores: `SystemStore` (info, health every 15 s, metrics every 2 s on the
  dashboard only, paused while the window is hidden), `EventLogStore`
  (subscribe first, then load history, dedupe by id), `SettingsStore`,
  `ModelsStore` (providers, catalog, routes, provider tests), `ChatStore`
  (conversation list and the open chat; streaming state machine
  `idle → connecting → streaming → idle`; "Automatic" lets the core route),
  `MemoryStore` (facts, projects, action history, search, data),
  `AgentsStore` (tasks, pushed task updates, approvals, tools, audit log).
- Chat stream callbacks and send acknowledgements are scoped to their local
  assistant entry. Finished, disconnected or disposed turns cannot change a
  later turn. A failed core health probe ends frontend waiting and preserves
  partial text with an explicit unknown-outcome message; it neither resends nor
  cancels the native request. Saved conversations can be reopened after recovery
  to inspect the persisted result. Detection follows the visible-window health
  poll, not a token inactivity timer. New chats and sends invalidate pending
  conversation-open reads so old history cannot replace a new turn.
  Stop commands are coalesced per active turn and have a 30-second frontend
  deadline, an independent pending indicator and retryable error. Acknowledgement
  only confirms the command; final stream events determine the reply's status.
  Refused or timed-out stops do not fabricate completion. Early cancellation is
  dispatched once when the request ID becomes known; old cancellation results
  cannot alter a subsequent turn, even if the core reuses its request ID.
  Opening a conversation has a 30-second frontend deadline. Failure retains the
  currently displayed chat; a later open can retry. Older timeouts cannot clear a
  newer open's loading state, and store destruction settles pending opens without
  applying their late results.
  Conversation search uses request generations, invalidated immediately on input
  changes, so even A → B → A edits during debounce cannot publish an old response.
  Reads have a 30-second frontend deadline and settle on store destruction.
  Search loading/errors are separate from chat errors and expose a retry action;
  failed refreshes retain existing rows. Confirmed rename/delete operations
  invalidate pending list reads, and destroying the list panel clears its debounce
  timer. Only a current successful post-turn list refresh may update the title.
  Conversation metadata writes are guarded per conversation to prevent overlapping
  full-record responses from reverting another edit. Responses update the open
  conversation and its error only in the original navigation context. Note saves
  share a separate write guard, block sending during draft-chat creation, and do
  not attach a newly created chat after navigation. The note editor retains edits
  made during a save and marks success only for the submitted draft. Native writes
  are not retried automatically or assumed cancelled when the user navigates away.
  Conversation deletion shares the metadata write guard and rejects deletion of
  the actively streaming chat. Pending deletion blocks sending into that chat.
  Its result distinguishes failure, background deletion and closing the current
  chat; the sidebar clears the route only for the last case. A later navigation
  is preserved, and pending reads cannot reopen a confirmed-deleted conversation.
  Failures keep the chat and rows available for retry.
  The rename editor remains open until a confirmed save of its submitted draft.
  Failed writes expose an inline retry message, Enter/blur share one pending
  write, and edits made during saving are retained. Editor session generations
  prevent a late response from closing a cancelled-and-reopened editor.
- **Agents screen**: new task (goal, model, limits), task list, the journal
  of the selected task with plan, steps, decisions and results, pause /
  resume / cancel; *Tools & permissions* (workspace folders, tools by risk,
  approvals, programs, network, limits, cloud permission); *Audit log* with
  an integrity check. The **approval dialog** is part of the app shell, so a
  request reaches the user on every screen. Its task link selects the requesting
  task; decision buttons lock while IPC is pending. A failed decision stays
  visible in that request's dialog and can be retried. Live approval events
  take precedence over a delayed initial queue snapshot.
- **Chat** keeps a history of saved and temporary conversations (search,
  rename, delete), a project and a pinned note per conversation, shows how
  each reply's context was built and lets the user save a message as a fact.
- **Run as agent** sends the composer text as an agent goal with the selected
  model (or automatic agent routing) and default task limits. The UI explains
  that tasks are saved separately, even from temporary chats. It does not send
  conversation history, pinned notes or project instructions. A failed start
  preserves the draft; a successful start opens `/agents?task=<id>`. Existing
  core permissions and approval dialogs apply unchanged.
  Agent list and journal reads retain task events received while IPC is pending,
  including events with equal timestamps. Only the latest list/selection request
  can publish its response or error. A start response also preserves events
  already received for the created task. Full event payloads are retained only
  for the lifetime of in-flight reads, rather than in a permanent journal cache.
  Task list, journal, status, tools and pending-approval snapshot reads have a
  30-second frontend deadline, including recovery reads. Destruction settles
  waiting reads without applying late results; timeout allows retry without
  cancelling native work. Status and tool responses also respect request order.
  A new operation batch clears its old error at startup, while sibling or parent
  success cannot erase a failure within the batch (including automatic journal
  selection after a list read). A live journal event remains authoritative if
  its pending snapshot times out.
  Agent subscriptions share one pending connection attempt. Failed initialization
  releases partial listeners before retry; store destruction also releases active
  or late-registering listeners. Queued callbacks from disposed attempts are ignored.
  Each task/approval listener registration has a 30-second frontend deadline.
  Timeout releases any partial listeners and permits retry. The native registration
  promise retains its cleanup handler, so a listener created after timeout or
  destruction is immediately released instead of leaking. Obsolete attempts cannot
  overwrite a newer connection's error or recovery state.
  Connection errors have a separate signal and a retry action on the Agents page,
  so successful background reads cannot hide a failed live subscription.
  Retrying a failed connection also reloads the task list and currently open
  journal after listeners are installed. Events received during recovery take
  precedence; selection changes and newer list requests are respected. A missing
  task clears its stale journal. Other recovery failures keep the connection
  error and retry action visible, and responses after disposal are ignored.
  The app's existing visible-window health poll invalidates agent subscriptions
  on a failed probe and retries them after a successful probe. Old approval
  prompts are withdrawn until the pending queue is reloaded. An unresolved old
  registration cannot block a new attempt; late callbacks remain inactive.
  Recovery never starts, resumes or cancels a native task. Detection follows the
  health polling interval (15 seconds while visible), not individual event silence.
  A health IPC request has a 10-second frontend deadline, above the core's
  six-second maximum probe budget. Concurrent callers share the same result.
  Timeout releases the frontend request slot for the next poll; late success
  or rejection cannot overwrite current health. Destruction settles the waiter
  and clears its timer. This does not cancel an already-dispatched native command.
  Resource sampling uses the same deadline mechanism and an independent request
  slot. Metrics failures clear stale samples and appear in the Resources panel;
  they do not mark the core offline or trigger agent reconnection. A successful
  sample clears only the metrics error, not an outstanding health-check failure.
  System event registration and history reads each have a 30-second frontend
  deadline and settle on store destruction. Late listener registration releases
  its resource, while obsolete history responses and callbacks are ignored.
  Failed reads preserve displayed events. The Events page shows connection
  progress and a retry action rather than an empty-log message during failure.
  The same health poll reconnects the system event log. A fresh history snapshot
  replaces the previous log and merges events received during loading, with live
  events taking precedence by ID. This prevents mixing sessions after the core's
  event counter resets. Both the displayed log and the temporary live buffer are
  capped at 500 entries. Failed or disposed attempts release their listener and
  cannot publish late history; concurrent callers share one connection attempt.
  Visibility-aware polling removes interval timers while the window is hidden
  and performs an immediate refresh when it becomes visible. This applies to
  health/reconnection, resource metrics, runtime status and the clock. A pending
  poll is not duplicated or cancelled; polling resumes on a later tick after it
  settles. Destruction removes both the timer and visibility listener. Unexpected
  polling failures go through Angular's error handler and do not stop later ticks.
  Runtime status shares the frontend deadline helper with health and metrics
  (10 seconds). Failed reads retain the last snapshot with an explicit stale-data
  notice, separate from action errors. Recovery clears only the read error.
  After a runtime action, an earlier poll is awaited before requesting a fresh
  snapshot. Late timed-out responses cannot roll back status or invalidate the
  model catalog. These deadlines do not limit model downloads or server startup.
  Runtime mutations are single-flight per resource (server role, model removal,
  download cancellation, engine selection/installation, import dialog or download
  submission). The first command owns that resource until the command and status
  refresh settle; further calls share its result and do not queue another action.
  Matching UI controls are disabled and a pending notice is shown. Independent
  resources remain usable, and failures release the resource for an explicit retry.
  Command results report their own success independently of concurrent failures
  or a failed follow-up status read. Action errors are retained per resource with
  a readable action label; only retrying that resource or dismissing its message
  clears the error. Custom model submissions clear the form only on their own
  success and only when its values still match the submitted values.
  SettingsStore admits one settings write at a time; overlapping submissions
  return false without sending another full snapshot.
  The native workspace-folder picker shares this write guard
  for its full lifetime, preventing duplicate dialogs and overlapping settings
  saves. Cancellation leaves settings and the tool-policy draft unchanged;
  success publishes the returned settings and merges folders into the draft
  without replacing other edits. Folder errors use the shared settings error.
  Policy saves take folders from confirmed settings, not the draft, so delayed
  agent-status refreshes cannot cause a later save to drop an added folder.
  Native file/folder dialogs (workspace folders, runtime binary/model selection,
  memory import/export) use the dialog plugin's callback API and a Tokio oneshot
  bridge. Waiting for user input does not block an async executor thread. User
  cancellation remains `None`; a dropped callback reports `unavailable`. A late
  callback after the IPC future is dropped is safely ignored. Dropping the future
  does not close the OS dialog, and there is no timeout on user selection.
  A write invalidates older settings reads, and reads started during a write are
  skipped. Failed writes
  preserve the last confirmed settings and release the save guard for a retry.
  The Runtime, general Settings, memory and tool-policy forms compare current values with their
  submitted snapshot before marking the form pristine, preserving edits made
  while the write was pending. Runtime exposes shared saving and error states.
  Memory and tool-policy saves refresh their dependent status after success even
  when the draft has newer edits. Tool program lists are normalized only when the
  submitted draft is still current. Discard is disabled while these saves run.
  Runtime release discovery is a separate read with a 30-second frontend deadline.
  Duplicate checks for the current backend share a request; changing the backend,
  dismissing the result, or leaving the Runtime screen invalidates its generation.
  Late results and errors cannot replace the current release or clear a newer
  loading state, including CPU-to-Vulkan-to-CPU changes. This does not cancel the
  native request. Hardware recommendations apply once and never override a manual
  backend choice. Finishing an installation clears only its own release result.
  Background model downloads reserve their destination atomically in the native
  registry, independent of display labels or catalog/custom entry points. Windows
  reservations ignore path case. Cancellation retains the reservation until the
  worker exits, protecting the shared partial file. The catalog UI disables
  known active transfers (including verification); native reservation remains
  authoritative when the UI snapshot is stale or labels differ.
- **Memory screen**: facts (pin, scope, edit), projects (instructions),
  action history, search with highlights, database status, memory settings,
  export/import and deleting everything after typing a confirmation word.
  Export, import and clear share a store-level operation guard through the native
  dialog, database work and post-import/clear refresh. Duplicate calls of the
  same operation share completion; conflicting calls are not queued. Transfer
  errors have a dedicated UI signal that background reads cannot clear. Starting
  a new transfer removes the previous report, including when the dialog is later
  cancelled. Parallel/nested memory reads retain failures until a new request
  batch starts, so successful siblings cannot conceal a failed import refresh.
  Memory search has its own loading/error state, request generation and 30-second
  frontend deadline. Identical pending queries share completion; changed queries
  or filters supersede older reads. Editing the search draft, leaving its panel,
  importing or clearing memory invalidates results and late responses. Empty text
  or no selected categories sends no search request. The deadline does not cancel
  the native search; retries cannot be overwritten by its eventual response.
  Fact and project lists track independent read generations. Superseded reads
  cannot replace newer lists or publish obsolete errors. Successful mutations
  invalidate earlier reads, apply the confirmed saved/deleted item immediately,
  and invalidate search results before refreshing dependent lists. If that
  refresh fails, the confirmed mutation remains visible with the read error.
  List responses received after store destruction are ignored.
  History pagination shares duplicate pending page reads. A first-page refresh
  supersedes an older next-page request; load-more waits for a pending refresh
  and sends no request once the end is known. Pages deduplicate episode IDs.
  History reads have a 30-second frontend deadline, preserving the displayed
  list on failure. Clearing history invalidates pending pages, shares duplicate
  deletion calls and prevents reads until deletion finishes. Import/clear-all
  also invalidate history reads before reloading. Late results and stale errors
  cannot restore removed history or reset a newer loading indicator.
  History uses an exclusive `(occurredAtMs, id)` cursor, ordered by timestamp and
  UUID descending. Equal-millisecond entries therefore span pages without gaps;
  the cursor does not require its boundary row to still exist. Schema migration
  3 replaces the timestamp-only history index with a matching composite index,
  preserving rows and using the storage layer's automatic pre-upgrade backup.
- **Models screen** (AI Model Manager): add/edit/enable/remove providers with
  a connection test, model list with capabilities and context length,
  per-category routing.
- **Markdown** in assistant messages (`marked` + `highlight.js`, lazy-loaded
  with the chat). Model output is untrusted: raw HTML is shown as text, links
  are inert (no navigation), images become alt text, and Angular sanitizes the
  result again; the CSP blocks scripts and remote images regardless.
- Unhandled UI errors are reported to the core (throttled) so they appear in
  logs and the event log.
- Design tokens in `styles.scss` (graphite background, blue/teal accents),
  system fonts only (nothing is downloaded), `prefers-reduced-motion` and a
  *reduce motion* setting.

## 5. Cross-cutting concerns

| Concern | Current approach |
| --- | --- |
| Async | Tokio (Tauri's runtime). Blocking work (`sysinfo`) on `spawn_blocking`. No locks held across `.await`. |
| Cancellation | `CancellationToken` per chat request, child of a shutdown token. |
| Timeouts | Health probes 3 s, model listing 5 s, chat overall + idle timeouts from settings. |
| Resource limits | ≤ 4 concurrent chats, ≤ 2 agent tasks, agent step/time limits, tool output/input limits, bounded event history, bounded input sizes, UI error rate limit (20/min). |
| Errors | `thiserror` enums → stable `ErrorCode`; no `unwrap`/`expect` outside tests (clippy-enforced). |
| Unsafe code | `unsafe_code = "forbid"` workspace-wide. |
| Observability | `tracing` + rotating files; user-facing event log. |

## 6. Known limitations

- Cloud providers (OpenAI, Anthropic, Gemini) use API keys stored in the OS
  credential vault. Linux requires an unlocked Secret Service session.
  Protocol mocks are tested; live cloud API compatibility requires credentials.
- The embedded runtime installs CPU and Vulkan builds; CUDA/ROCm/SYCL builds
  must be selected manually. The real llama.cpp engine is verified manually,
  automated tests use a fake server.
- A server outlives JARVIS if JARVIS is killed forcibly; the next start
  stops it.
- Capabilities of OpenAI-compatible models are inferred, not reported.
- Tested against protocol mocks; compatibility with every server version is
  not guaranteed.
- Token counts are estimated, not tokenized; the budget keeps a margin.
- Semantic search needs an embedding model: the runtime's embedding server
  (BGE-M3 in the catalog) or a provider such as Ollama. The embedding server
  takes at most 2 048 tokens per text (less for models trained with a
  shorter context, e.g. 512 for multilingual-e5; longer texts then fail to
  index). No query/passage prefixes are added.
- Similarity cuts are measured for BGE-M3 only (ADR 0009: unrelated facts
  score up to 0.61, median 0.39; floors 0.52 for search and 0.55 for recall,
  margin 0.10 below the best hit). Other embedding models keep the earlier,
  unmeasured floors (0.35 / 0.50) with the same margin. Facts the model
  places close to the query (e.g. two allergies, two appointments) still
  match; weaker relevant matches below the floor are found only by words.
- Without an embedding model, a question that shares one word with an
  unrelated fact still finds it ("Where do I work?" → "…works as a
  teacher"): words alone cannot tell whose work is meant.
- Vector search is brute force in memory (fine for tens of thousands of
  items).
- The memory database is not encrypted at rest.
- Facts are saved explicitly; JARVIS does not extract them automatically.
- Tools are not sandboxed: programs the user allowed and approved run with
  the user's rights. The policy limits what tools are asked to do.
- Agents use a JSON text protocol, not native tool calling; small local
  models follow it imperfectly (invalid replies end a task after three
  tries).
- The audit log detects changes but cannot stop someone with access to the
  database from rewriting it entirely.
- UI text is English; the assistant language sets the language of replies
  and of speech.
- Voice: no echo cancellation of its own (laptop microphone arrays often
  cancel the speakers themselves); the wake word is recognized by Whisper,
  which keeps a CPU core busy while people talk nearby; Whisper Small needs
  4–7 s per phrase on a laptop CPU.
- Vision: X11 lets any program read the screen, so JARVIS relies on its own
  UI to capture only on request; Wayland portals may keep a copy of each
  screenshot; local vision models answer a screenshot in about a minute on
  a CPU. The camera works where the WebView offers one (not WebKitGTK).
- Linux builds are verified in CI only (not on the development machine).
- No end-to-end (WebDriver) tests yet.

## 7. Planned architecture (not implemented)

See [ROADMAP.md](ROADMAP.md) for phases. Key future boundaries:

- `jarvis-security`: process isolation for tools.
- `jarvis-tools`: native tool calling where providers support it, MCP tools.
- `jarvis-voice`: echo cancellation, optional cloud speech, a dedicated
  wake-word model.
- `jarvis-sync`: optional Supabase sync with an offline queue.


## Provider credentials

`jarvis-security::SecretStore` separates the OS vault from test doubles.
The production dependency enables `windows-native` on Windows and
`sync-secret-service` with `crypto-rust` on Linux. The UI can set, check for,
or delete a key; it cannot retrieve saved secrets. Keys are kept out of settings.
Set/delete operations rebuild the provider registry and invalidate the model
catalog. Existing requests retain their provider; subsequent requests use the
updated credential. Model-list and authentication errors propagate to connection
tests instead of being replaced by a hard-coded catalog.

All core-owned native vault operations run on Tokio's blocking pool, including
reads during initialization, registry reloads and connection tests. Registry
reloads remain serialized and publish a complete provider snapshot only after
credential reads finish. An OS vault prompt can delay that operation, but does
not occupy an async executor thread. Native blocking calls cannot be forcibly
cancelled once started.

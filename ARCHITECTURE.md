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
│ jarvis-core  → composition root: config, health, chat         │
│ jarvis-ai    → AiProvider trait, registry, providers          │
│ jarvis-events→ event bus + system event log                   │
│ jarvis-platform → OS adapters, system monitor                 │
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
                              PlatformInfo, SystemMonitor (sysinfo)
  jarvis-ai/                  AiProvider trait, ProviderRegistry, EchoProvider
  jarvis-core/                JarvisCore, Settings/ConfigStore, health checks,
                              ChatService, error codes
apps/desktop/
  src/                        Angular UI
    app/core/ipc/             TauriBridge (only user of @tauri-apps/api), JarvisApi
    app/core/state/           signal stores (system, events, settings, chat)
    app/core/errors/          global error handler → core log
    app/features/             dashboard, chat, events, settings (lazy loaded)
    app/shared/               HUD panel, AI core, icons, status badge, formatters
  src-tauri/                  Tauri 2 shell (crate jarvis-desktop)
    capabilities/             IPC permissions per window
    build.rs                  app command manifest, Windows manifest
packages/shared-types/        TypeScript IPC types generated from Rust (ts-rs)
docs/decisions/               Architecture Decision Records
.github/workflows/ci.yml      Windows + Linux CI
```

Crates that exist only on the roadmap (`jarvis-memory`, `jarvis-storage`,
`jarvis-voice`, `jarvis-vision`, `jarvis-tools`, `jarvis-security`,
`jarvis-agent`, `jarvis-runtime`, `jarvis-sync`) are **not created yet**; they
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
| `list_models()` | models per provider; per-provider errors, 5 s timeout |
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
AI providers. Each probe has a 3 s timeout. The AI component is `healthy` only
if at least one *real* provider answers; with only the diagnostic provider it
is honestly `degraded`.

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

Only `EchoProvider` exists today: a clearly labelled *diagnostic* provider that
streams the user's message back word by word to exercise streaming,
cancellation and error paths. See [ADR 0003](docs/decisions/0003-ai-provider-abstraction.md).

### 4.3 Chat orchestration (`jarvis-core::chat`)

`ChatService::start(request, sink)`:

1. validates input (≤ 200 messages, ≤ 32 000 chars each, ≤ 256 000 total,
   at least one user message, temperature 0–2, …);
2. resolves provider (request → settings → first real provider → diagnostic);
3. **privacy policy:** rejects `thirdPartyCloud` providers unless
   `privacy.allowCloudProviders` is enabled (logged as a security event);
4. resolves the model and checks its `chat` capability;
5. enforces ≤ 4 concurrent requests;
6. spawns a task that streams `ChatStreamEvent`s to the sink with an overall
   timeout (`ai.requestTimeoutSecs`) and an idle timeout between chunks
   (`ai.idleTimeoutSecs`), honouring cancellation.

Every accepted request ends with exactly one `finished` (reason `stop`,
`length` or `cancelled`) or `failed` event, unless the receiver disappears.

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
| `list_ai_models` | – | `ProviderModels[]` |
| `start_chat` | `request: ChatStartRequest`, `onEvent: Channel<ChatStreamEvent>` | request id |
| `cancel_chat` | `requestId: string` | `boolean` |
| `report_ui_error` | `report: UiErrorReport` | – |

Event: `jarvis://system-event` → `SystemEvent`.

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
  `ChatStore` (streaming state machine `idle → connecting → streaming → idle`).
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
| Resource limits | ≤ 4 concurrent chats, bounded event history, bounded input sizes, UI error rate limit (20/min). |
| Errors | `thiserror` enums → stable `ErrorCode`; no `unwrap`/`expect` outside tests (clippy-enforced). |
| Unsafe code | `unsafe_code = "forbid"` workspace-wide. |
| Observability | `tracing` + rotating files; user-facing event log. |

## 6. Known limitations (Phase 1)

- No real AI provider yet; only the diagnostic echo provider.
- Conversations live in UI memory only; nothing is persisted (Phase 4).
- The UI sends up to the last 40 messages as context; token-aware context
  management arrives with the Memory Engine.
- UI text is English; the assistant language setting is stored but not used
  until real models and voice exist.
- Linux builds are verified in CI only (not on the development machine).
- No end-to-end (WebDriver) tests yet.

## 7. Planned architecture (not implemented)

See [ROADMAP.md](ROADMAP.md) for phases. Key future boundaries:

- `jarvis-runtime`: managed `llama-server` child process (GGUF import,
  checksums, hardware detection, safe termination).
- `jarvis-storage` / `jarvis-memory`: SQLite with migrations, FTS, vector
  search, layered memory, context manager.
- `jarvis-security`: policy engine (capabilities, allowlists, scoped file
  access, approvals, audit log, OS keychain for secrets).
- `jarvis-tools` / `jarvis-agent`: tool registry, planner/executor/verifier,
  task states, step/time/resource limits.
- `jarvis-voice` / `jarvis-vision`: STT/TTS/VAD/wake word; screen capture via
  OS-specific adapters (including xdg-desktop-portal on Wayland).
- `jarvis-sync`: optional Supabase sync with an offline queue.

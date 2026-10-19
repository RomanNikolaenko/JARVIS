# Roadmap

Legend: ✅ done · 🟡 partially done · ⬜ not started

## Phase 1 — Foundation ✅

- ✅ Rust workspace (edition 2024), crates `jarvis-core`, `jarvis-ai`,
  `jarvis-events`, `jarvis-platform`
- ✅ Tauri 2 shell + Angular 22 UI (zoneless, signals, lazy routes)
- ✅ Typed IPC with generated TS types, structured errors, deny-by-default ACL
- ✅ Health check, settings (TOML, validation, atomic writes), logging, event log
- ✅ Dark HUD UI: Dashboard, Chat (streaming + stop), Event log, Settings
- ✅ Tests (Rust unit/integration/IPC, Angular unit), Windows + Linux CI
- ⬜ End-to-end UI tests (WebDriver) — moved to Phase 8
- ⬜ Manual verification on a Linux desktop (X11 and Wayland)

## Phase 2 — AI integration 🟡

- ✅ `AiProvider` abstraction, capabilities, privacy locality, registry
- ✅ Streaming chat pipeline with cancellation and timeouts (diagnostic provider)
- ⬜ Ollama provider (`/api/chat`, `/api/tags`, streaming NDJSON)
- ⬜ OpenAI-compatible provider (LM Studio, llama.cpp server, vLLM, …)
- ⬜ Provider configuration UI (endpoints, health, models)
- ⬜ Model router (per-category model selection, capability/privacy aware)
- ⬜ Provider contract test suite (shared by all providers, mock HTTP server)
- ⬜ Markdown rendering with sanitization and code highlighting in chat
- ⬜ Secrets in the OS keychain (for cloud providers)

## Phase 3 — Embedded runtime ⬜

`jarvis-runtime`: managed `llama-server` process, GGUF import, verified
downloads with checksums and progress, hardware detection (CPU/GPU,
RAM/VRAM), context size/threads configuration, safe start/stop.

## Phase 4 — Memory ⬜

`jarvis-storage` (SQLite + migrations), conversation history, memory layers
(working, conversation, semantic, project, episodic), FTS + vector search,
Context Manager (token budgets, summarization, compaction), export/import,
"don't remember" mode.

## Phase 5 — Tools and agents ⬜

Tool registry, Security Policy Engine, approvals, audit log, task manager
(pending/running/waiting for approval/completed/failed/cancelled), agent
orchestrator (planner, executor, verifier, recovery) with step/time/resource
limits.

## Phase 6 — Voice and vision ⬜

STT (whisper.cpp), TTS, VAD, wake word «Джарвіс», device selection, barge-in;
screen/window capture per platform (portal on Wayland), camera, vision models.

## Phase 7 — Sync and extensions ⬜

Optional Supabase auth/sync (offline queue, conflict resolution, encryption,
per-category opt-in), plugin API, MCP integration.

## Phase 8 — Production readiness ⬜

Performance work, security review, signed installers, auto-updates, E2E tests,
release testing, ARM64 builds.

## Next iteration

Phase 2: Ollama provider + OpenAI-compatible provider with a shared contract
test suite, provider settings in the UI, and Markdown rendering in chat.

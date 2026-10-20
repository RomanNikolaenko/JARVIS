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
- ✅ Streaming chat pipeline with cancellation and timeouts
- ✅ Ollama provider (`/api/chat` NDJSON, `/api/tags`, `/api/show`, `/api/embed`)
- ✅ OpenAI-compatible provider (LM Studio, llama.cpp server, vLLM, …)
- ✅ Provider configuration UI (Models screen: add/edit/test/enable/remove)
- ✅ Model router (per-category selection, capability/privacy/locality aware)
- ✅ Provider contract test suite (shared by all providers, scripted HTTP server)
- ✅ Markdown rendering with inert links/images and code highlighting
- ⬜ Secrets in the OS keychain; API-key authentication
- ⬜ Cloud providers (OpenAI, Anthropic, Gemini) behind the privacy opt-in
- ⬜ Verification against real Ollama / LM Studio installations (only protocol
  mocks so far)

## Phase 3 — Embedded runtime 🟡

- ✅ Hardware detection: CPU features, RAM, GPUs (NVIDIA via nvidia-smi,
  Windows registry, Linux sysfs), Vulkan availability, free disk space
- ✅ Verified installation of official llama.cpp builds (CPU/Vulkan) with
  explicit approval, digest check and safe extraction; or an existing
  `llama-server`
- ✅ GGUF library: import in place, header validation, memory estimate
- ✅ Verified model downloads (curated catalog, any public Hugging Face GGUF),
  resume, cancel, disk space check
- ✅ Supervised server: 127.0.0.1 + per-launch API key, readiness, crash
  detection, safe stop, orphan cleanup, memory check, GPU layers, threads,
  context size, autostart (opt-in)
- ✅ Runtime screen; embedded provider in routing and health
- ✅ Verified with the real llama.cpp engine on Windows (b11739, Qwen2.5 3B)
- ⬜ Verification with the real engine on Linux
- ⬜ CUDA build installation (needs the separate CUDA runtime archive)
- ✅ Embedding model serving: second supervised server (`--embedding`,
  pooling from GGUF), separate start/stop and autostart, BGE-M3 in the
  catalog (live check with the real engine: pending)

## Phase 4 — Memory 🟡

- ✅ `jarvis-storage`: SQLite (bundled, FTS5), versioned migrations with
  checksums, downgrade protection, backups before migrating
- ✅ Memory layers: working (pinned note + recent turns), conversation
  (history + summaries), semantic (facts, pinned/relevant, per project),
  project (instructions), episodic (action history)
- ✅ Hybrid search: FTS5 + vector similarity (embeddings from a provider),
  reciprocal rank fusion; text-only fallback
- ✅ Context Manager: per-model token budgets, tiered context, compaction by
  the serving model, reasoning stripped
- ✅ Conversation history UI, temporary chats, Memory screen, export/import,
  delete selected items or everything, "don't remember" mode, cloud sharing
  opt-in
- ✅ Embedding model served by the embedded runtime: semantic search works
  without Ollama (indexing starts when the embedding model comes up)
- ⬜ Encryption at rest (key in the OS keychain)
- ⬜ Automatic fact extraction (opt-in)

## Phase 5 — Tools and agents 🟡

- ✅ `jarvis-security`: Security Policy Engine (capabilities, risk levels,
  tool switches, scoped file access with protected JARVIS folders, secret
  files, program allowlist, SSRF guard), approvals with timeout, hash-chained
  audit log
- ✅ `jarvis-tools`: two-phase tools (prepare → policy → run) — files (list,
  read, search, write, edit, copy, move, delete), programs without a shell
  (run, launch, stop), system info, processes, web fetch/open, read-only git
- ✅ `jarvis-agent`: task manager (pending, running, waiting for approval,
  paused, completed, failed, cancelled; persisted journal; recovery after
  restarts), orchestrator (planner, executor, verifier, recovery by the
  model), step/time/failure limits, no automatic retries
- ✅ Agents screen (tasks, journal, pause/resume/cancel), Tools & permissions,
  audit log with integrity check, approval dialog on every screen
- ✅ Cloud models for agents behind a separate permission
- ⬜ Process isolation for programs (AppContainer / bubblewrap / Landlock)
- ⬜ Native tool calling for providers that support it
- ⬜ Tools from MCP servers and plugins (Phase 7)
- ⬜ Agents started from chat (today: the Agents screen)

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

Live verification of Phase 5 on Linux, embeddings in the embedded runtime,
then Phase 6 (voice and vision). Open Phase 2 items: keychain-backed API keys
and cloud providers.

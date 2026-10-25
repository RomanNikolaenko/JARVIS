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
- 🟡 Manual verification on Linux: done in WSL (Ubuntu 24.04, WSLg/Wayland);
  a physical X11/Wayland desktop is still to do

## Phase 2 — AI integration 🟡

- ✅ `AiProvider` abstraction, capabilities, privacy locality, registry
- ✅ Streaming chat pipeline with cancellation and timeouts
- ✅ Ollama provider (`/api/chat` NDJSON, `/api/tags`, `/api/show`, `/api/embed`)
- ✅ OpenAI-compatible provider (LM Studio, llama.cpp server, vLLM, …)
- ✅ Provider configuration UI (Models screen: add/edit/test/enable/remove)
- ✅ Model router (per-category selection, capability/privacy/locality aware)
- ✅ Provider contract test suite (shared by all providers, scripted HTTP server)
- ✅ Markdown rendering with inert links/images and code highlighting
- ✅ Secrets in the OS keychain; API-key authentication
- ✅ Cloud providers (OpenAI, Anthropic, Gemini) behind the privacy opt-in
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
- ✅ Verification with the real engine on Linux (WSL Ubuntu 24.04: official
  b11782 CPU build installed through the app, Qwen2.5 3B chat)
- ⬜ CUDA build installation (needs the separate CUDA runtime archive)
- ✅ Embedding model serving: second supervised server (`--embedding`,
  pooling from GGUF), separate start/stop and autostart, BGE-M3 in the
  catalog; verified with the real engine on Windows (b11739, BGE-M3 Q8_0
  next to Qwen2.5 3B, CPU)

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
- ✅ Similarity cuts per embedding model (floor + margin below the best
  hit), measured for BGE-M3 on Ukrainian and English facts; other models
  not measured yet
- ✅ Text matches judged by shared terms: fewer than the best hit's are
  dropped, one shared word of a question yields to the semantic side
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
- ✅ Tools from MCP servers and plugins (Phase 7)
- ✅ Agents started from chat: composer action uses the selected model and
  opens the saved task journal; conversation history is not sent

## Phase 6 — Voice and vision 🟡

- ✅ `jarvis-voice`: audio capture and playback (cpal), VAD (earshot),
  speech recognition with a managed whisper.cpp server, speech synthesis
  with a long-running Piper process (uk + en voices), wake word «Джарвіс» /
  "Jarvis", push-to-talk, streamed replies read sentence by sentence,
  barge-in, device selection
- ✅ Pinned and checksummed engine/model/voice packages, installed only
  after confirmation; program integrity checked before every start
- ✅ Microphone indicator on every screen and in the event log; wake word
  mode is opt-in
- ✅ `jarvis-vision`: screen and window capture on Windows and X11,
  portal dialog on Wayland, capability reporting; image files and camera
  photos (WebView camera)
- ✅ Images for vision models (OpenAI-compatible, Ollama, llama.cpp with
  `--mmproj`; curated Qwen3-VL 2B and Gemma 3 4B), vision routing,
  separate permission for sending captures to cloud models
- ✅ UI: microphone button with level meter, voice status, Settings →
  Voice, attachments with previews in the chat
- ✅ Live check on Windows: verified installs, speech, spoken replies,
  microphone, wake word mode switch, screenshot and window capture with
  Qwen3-VL 2B (found and fixed: a crash in synchronous IPC commands,
  microphone closing on a WASAPI glitch, 20 s recognition per phrase)
- ⬜ Live check on Linux; wake word with a live voice
- ⬜ Cloud speech (optional), echo cancellation, a dedicated wake-word model
- ⬜ Camera on Linux (WebKitGTK media capture is off in Tauri)

## Phase 7 — Extensions 🟡

All data stays on the user's computer: cloud sign-in and sync (Supabase)
were dropped ([ADR 0013](docs/decisions/0013-local-data-only.md)).

- ✅ Plugin API: a plugin is an MCP server with a `jarvis-plugin.json`
  manifest, or any MCP server entered as a command
  ([ADR 0012](docs/decisions/0012-plugins-and-mcp.md))
- ✅ `jarvis-plugins`: MCP client for stdio servers, protocol 2026-07-28
  (`server/discover`, `_meta`, `input_required`) with fallback to the
  `initialize` handshake of 2025-11-25 and older; message size limits,
  deadlines, cancellation
- ✅ Consent: review before the first start, fingerprint of the program and
  its files checked before every start, warnings for launchers like `npx`;
  secrets in the OS keychain; bounded automatic restarts
- ✅ Plugin tools in the agent registry behind the Security Policy Engine:
  high risk by default, per-tool on/off and risk, risk reset when a tool's
  definition changes, never repeated automatically
- ✅ Plugins screen; example plugin `examples/plugins/text-tools`
- ✅ Live check on Windows: the example server through the Plugins screen,
  an agent (Qwen2.5 3B) calling a plugin tool after approval, a changed
  script stopped until reviewed again, disable and remove
- ✅ Tool list changes of modern servers through `subscriptions/listen`
  (checked with `server-everything`'s `trigger-list-changed`)
- ⬜ HTTP (remote) MCP servers, resources and prompts
- ✅ Stopping a plugin stops what its server started: process group with
  `SIGTERM` → `SIGKILL` on Unix, process tree of a stuck server on Windows
- ⬜ Sandboxed plugin processes; job objects on Windows (programs left
  behind by a server that exited on its own)
- ✅ Interoperability with the official reference servers
  (`@modelcontextprotocol/server-everything` and `server-filesystem` 2.0.1,
  protocol 2026-07-28): tools listed and called, images and resource links
  summarized, structured results, elicitation refused clearly, the server
  gets only the minimal environment; `mcp-probe` example for plugin authors
- ⬜ Live check on Linux

## Phase 8 — Production readiness 🟡

- ✅ Security review ([docs/security-review-2026-10.md](docs/security-review-2026-10.md)):
  IPC surface, CSP and rendering of model output, downloads, secrets, plugin
  event log fixed (no command arguments), development dependencies patched
  (`npm audit`: 0)
- ✅ Installers: NSIS (current user, no administrator rights) and MSI on
  Windows; deb, rpm and AppImage on Linux (ALSA dependency declared)
- ✅ Release workflow: installers on Windows and Ubuntu 22.04, release smoke
  test, draft GitHub release with `SHA256SUMS.txt` ([docs/RELEASE.md](docs/RELEASE.md))
- ✅ Release smoke test (`apps/desktop/e2e/smoke.mjs`) against the release
  binary with a temporary profile (`JARVIS_PROFILE_DIR`), in CI on every push;
  GitHub's Windows runners are elevated, where WebView2 150+ ignores the
  debugging port from the environment, so the workflows set it as a machine
  policy; WebView2 diagnostics as annotations when the endpoint is missing
- ✅ Performance: the idle dashboard used 7.5 % of all CPU cores (animated
  SVG and meter widths repainted on every frame); now 2.6 %, 1.1 % while
  another window has the focus (animations pause), under 0.1 % on other
  screens; the smoke test fails if the idle renderer gets busy again
- ✅ CHANGELOG, release guide
- ✅ Signed automatic updates (ADR 0014): `tauri-plugin-updater`, key pair
  outside the repository, signatures bound to the version, `latest.json`
  from the Release workflow, check after start and daily (setting), install
  only after confirmation; end-to-end test with a refused tampered file, a
  refused version mismatch and a real installer run
- ⬜ Authenticode-signed Windows installers (needs a certificate)
- ✅ RustSec scan: 6 advisories in the TLS stack fixed (`rustls` 0.23.46,
  `rustls-webpki` 0.103.16, `webpki-root-certs` 1.0.11); `cargo audit` and
  `npm audit` in CI on every push
- ⬜ License for the project
- ✅ ARM64 builds (Windows 11 ARM and Linux) in CI and the Release workflow,
  with the release smoke test (CI green on all four desktop targets, run
  40177361213) and `aarch64` update feed entries
- ⬜ Voice on ARM64 (pinned whisper.cpp and Piper packages for ARM64)
- ⬜ Native WebDriver end-to-end tests (the CDP smoke test covers Windows)

## Next iteration

Finish Phase 8: a license, then the manual release checklist and v0.1.0
(the update signing secrets are in GitHub, the RustSec scan runs in CI).
Open items: live checks on Linux (Phases 6 and 7), the wake word with a live
voice, third-party MCP servers.
Open Phase 2 items: live verification of local and cloud providers, including
the Linux Secret Service integration.

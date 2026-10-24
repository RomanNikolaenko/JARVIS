# J.A.R.V.I.S. — personal AI assistant

A local-first, privacy-first desktop AI assistant for **Windows 10/11** and
**Linux** (X11 and Wayland), built with **Rust**, **Tauri 2** and **Angular**.

> **Status: Phase 7 — Extensions.** JARVIS chats with local models
> served by **Ollama**, any **OpenAI-compatible** server (LM Studio,
> llama.cpp server, vLLM, …) or its own **embedded llama.cpp runtime**,
> remembers conversations, facts and projects in a local SQLite database,
> and runs **multi-step agent tasks** with tools (files, programs, system,
> web, git) behind a Security Policy Engine with approvals and an audit log.
> Cloud providers: OpenAI, Anthropic and Gemini with OS-keychain API keys.
> It listens and speaks (whisper.cpp, Piper, wake word «Джарвіс») and looks
> at screenshots, windows and photos with vision models — all on your
> computer. Plugins add tools through the Model Context Protocol (MCP).
> All data stays on your computer. See
> [ROADMAP.md](ROADMAP.md).

## What works today

- Desktop app (Tauri 2 + Angular 22) with a dark HUD-style UI:
  Dashboard, Chat (history, Markdown, code highlighting, stop), Models,
  Runtime, Agents, Memory, Plugins, Event log, Settings.
- Local AI: Ollama and OpenAI-compatible servers, auto-detected on their
  default ports; add servers on the Models screen with a connection test.
- Model routing per task (general, reasoning, coding, vision, embeddings,
  tool calling), automatic or explicit; cloud hosts blocked unless allowed.
- Memory: conversation history (saved or temporary chats), facts you ask
  JARVIS to remember, projects with instructions, an action history, search
  by words and meaning, export/import, delete anything; per-model context
  budgets with automatic summaries of long conversations.
- Agents (Agents screen): give JARVIS a goal; it plans, uses tools step by
  step and checks its answer, within step and time limits. Tools only see
  the workspace folders you add; read-only actions run on their own,
  changes and programs wait for your approval in a dialog; every decision
  is written to a tamper-evident audit log. Pause, resume or cancel any
  task; tasks survive restarts. Tools are not sandboxed — see
  [SECURITY.md](SECURITY.md).
- Start an agent directly from Chat with **Run as agent**. It uses the draft
  and selected model, then opens the task journal. Tasks are saved separately
  from the chat; chat history is not included.
- Embedded runtime (Runtime screen): install an official llama.cpp build
  (CPU/Vulkan) after explicit approval, import GGUF files or download
  verified models, start/stop a model with memory checks, GPU offload and
  hardware detection; a second server serves an embedding model (BGE-M3)
  for semantic memory search without Ollama.
- Voice (Settings → Voice): press the microphone button in the chat or say
  «Джарвіс» / "Jarvis" (wake word mode, off by default); speech is
  recognized by whisper.cpp and replies are read aloud by Piper in
  Ukrainian or English, interruptible at any time. A red indicator shows
  whenever the microphone is open. Engines, models and voices are verified
  downloads you approve one by one.
- Vision: attach a screenshot, a window, an image file or a camera photo to
  a message and ask a vision model about it (Ollama, OpenAI-compatible, or
  the embedded runtime with Qwen3-VL / Gemma 3). Captures happen only when
  you ask, you see them before sending, and they reach cloud models only
  with a separate privacy switch.
- Plugins (Plugins screen): add an MCP server or a plugin folder
  (`jarvis-plugin.json`); JARVIS shows exactly what it will run and a
  fingerprint of the files, starts it only after you confirm, and stops it
  if anything changes. Agents can use its tools; each call asks first until
  you lower a tool's risk. API keys of plugins live in the OS keychain.
  Plugins are not sandboxed — add only ones you trust. Example:
  [examples/plugins/text-tools](examples/plugins/text-tools).
- Rust core (`jarvis-core`) independent of the UI shell:
  - health check of all core components with timeouts;
  - settings in `config.toml` with validation, atomic writes and safe recovery;
  - CPU / RAM / process metrics and platform detection (incl. Wayland vs X11);
  - provider-agnostic AI layer with capability detection and privacy metadata;
  - streamed chat with cancellation, timeouts and concurrency limits;
  - system event bus and rotating diagnostic logs.
- Typed IPC: Rust types → generated TypeScript (`packages/shared-types`),
  structured error codes, deny-by-default command permissions.
- Tests: Rust unit + integration + IPC/ACL tests, Angular unit tests.
- CI for Windows and Linux.

## Quick start

Prerequisites: Rust (stable, ≥ 1.90), Node.js 24 LTS (or 22.22+), and the
[Tauri 2 system dependencies](https://v2.tauri.app/start/prerequisites/)
(WebView2 + MSVC Build Tools on Windows; `webkit2gtk-4.1` etc. on Linux).

```bash
cd apps/desktop
npm ci
npm run tauri dev
```

To chat with a real model, either open **Runtime** in JARVIS, install the
llama.cpp engine and download a model, or run [Ollama](https://ollama.com) /
LM Studio, which JARVIS finds on their default ports. Details:
[DEVELOPMENT.md → Local AI](DEVELOPMENT.md#local-ai).

Full instructions (Linux packages, tests, release builds, troubleshooting):
[DEVELOPMENT.md](DEVELOPMENT.md).

## Documentation

| Document | Contents |
| --- | --- |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Layers, crates, contracts, IPC, limitations |
| [docs/decisions/](docs/decisions/) | Architecture Decision Records |
| [DEVELOPMENT.md](DEVELOPMENT.md) | Setup, build, test, debug on Windows/Linux |
| [SECURITY.md](SECURITY.md) | Threat model, implemented controls, reporting |
| [ROADMAP.md](ROADMAP.md) | Phases and current progress |
| [CONTRIBUTING.md](CONTRIBUTING.md) | Workflow and quality gates |

## Repository layout

```
apps/desktop/          Angular UI (src/) and Tauri shell (src-tauri/)
crates/                Rust domain crates (core, ai, runtime, storage, memory,
                       security, tools, agent, plugins, voice, vision,
                       events, platform)
examples/plugins/      Example plugin (MCP server)
packages/shared-types/ Generated TypeScript IPC types
docs/decisions/        ADRs
```

## License

No license has been chosen yet. Until one is added, all rights are reserved by
the authors.

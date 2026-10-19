# J.A.R.V.I.S. — personal AI assistant

A local-first, privacy-first desktop AI assistant for **Windows 10/11** and
**Linux** (X11 and Wayland), built with **Rust**, **Tauri 2** and **Angular**.

> **Status: Phase 2 — AI integration (in progress).** JARVIS chats with local
> models served by **Ollama** or any **OpenAI-compatible** server (LM Studio,
> llama.cpp server, vLLM, …) on this machine or the local network, with
> streaming, cancellation, Markdown and per-task model routing. Not yet: API
> keys / cloud providers, the embedded llama.cpp runtime, memory, agents,
> voice. See [ROADMAP.md](ROADMAP.md).

## What works today

- Desktop app (Tauri 2 + Angular 22) with a dark HUD-style UI:
  Dashboard, Chat (Markdown, code highlighting, stop), Models, Event log,
  Settings.
- Local AI: Ollama and OpenAI-compatible servers, auto-detected on their
  default ports; add servers on the Models screen with a connection test.
- Model routing per task (general, reasoning, coding, vision, embeddings,
  tool calling), automatic or explicit; cloud hosts blocked unless allowed.
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

To chat with a real model, run [Ollama](https://ollama.com) (`ollama pull
llama3.2`) or LM Studio's local server; JARVIS finds both on their default
ports. Details: [DEVELOPMENT.md → Local AI](DEVELOPMENT.md#local-ai).

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
crates/                Rust domain crates (core, ai, events, platform)
packages/shared-types/ Generated TypeScript IPC types
docs/decisions/        ADRs
```

## License

No license has been chosen yet. Until one is added, all rights are reserved by
the authors.

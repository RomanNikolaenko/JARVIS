# J.A.R.V.I.S. — personal AI assistant

A local-first, privacy-first desktop AI assistant for **Windows 10/11** and
**Linux** (X11 and Wayland), built with **Rust**, **Tauri 2** and **Angular**.

> **Status: Phase 1 — Foundation.** The application shell, Rust core, typed
> IPC, health checks, settings, logging, system event log, dashboard and a
> streaming chat UI work. **No real AI model is connected yet**: chat uses a
> clearly labelled *diagnostic echo* provider that verifies streaming and
> cancellation. Local AI (Ollama, LM Studio, llama.cpp) is the next phase.
> See [ROADMAP.md](ROADMAP.md).

## What works today

- Desktop app (Tauri 2 + Angular 22) with a dark HUD-style UI:
  Dashboard, Chat, Event log, Settings.
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

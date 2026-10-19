# Development guide

## Toolchain

| Tool | Version used | Notes |
| --- | --- | --- |
| Rust | stable 1.98 (MSRV declared: 1.90) | `rustup component add rustfmt clippy` |
| Node.js | 24 LTS (22.22+ supported) | npm 11 |
| Tauri CLI | 2.12 (npm `@tauri-apps/cli`, no global install needed) | |
| Angular | 22.2, TypeScript 6.0 | Angular CLI via `npx ng` |

### Windows 10/11

1. Visual Studio Build Tools (or Visual Studio) with *Desktop development with
   C++* (MSVC + Windows SDK).
2. Microsoft Edge WebView2 Runtime (preinstalled on Windows 11).
3. Rust via `rustup` (target `x86_64-pc-windows-msvc`), Node.js 24.

### Linux (Ubuntu/Debian 24.04+)

```bash
sudo apt-get update
sudo apt-get install -y build-essential curl wget file libwebkit2gtk-4.1-dev \
  libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev
```

Fedora: `sudo dnf install webkit2gtk4.1-devel openssl-devel curl wget file
libappindicator-gtk3-devel librsvg2-devel libxdo-devel` and the
"C Development Tools and Libraries" group.

Both X11 and Wayland sessions work for the current features. Wayland-specific
limits (screen capture, global shortcuts) matter from Phase 6 on.

## Common tasks

All npm commands run in `apps/desktop`; cargo commands in the repository root.

```bash
# install frontend dependencies
cd apps/desktop && npm ci

# run the desktop app with hot reload (starts `ng serve` automatically)
npm run tauri dev

# frontend only (no IPC: the UI shows "Core offline")
npm start

# frontend unit tests (Vitest)
npm run test:ci

# frontend formatting
npm run format:check   # or: npm run format

# release binary without installers
npm run tauri -- build --no-bundle

# installers (NSIS/MSI on Windows; deb/rpm/AppImage on Linux)
npm run tauri build
```

```bash
# Rust: format, lint, test (from the repository root)
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

`cargo test` also regenerates the TypeScript IPC types (test `export_typescript_bindings` in `apps/desktop/src-tauri/src/bindings.rs`) in
`packages/shared-types/src/generated/`. Commit them together with the Rust
change; CI fails if they differ.

## Adding an IPC command

1. Implement the logic in a domain crate (usually `jarvis-core`), with tests.
2. Add a thin `#[tauri::command]` in `apps/desktop/src-tauri/src/commands.rs`.
3. Register it in `with_commands` (`src/lib.rs`) **and** in `COMMANDS`
   (`build.rs`).
4. Grant `allow-<command-name>` in `capabilities/main-window.json` (only if the
   main window needs it).
5. If the command introduces a new top-level argument/result type, derive
   `ts_rs::TS` with `#[ts(export_to = "<crate>.ts")]` and add it to
   `src-tauri/src/bindings.rs`; run `cargo test` to regenerate the TS types.
6. Add the typed method to `JarvisApi` (`src/app/core/ipc/jarvis-api.ts`).
7. Extend the IPC tests (`src-tauri/src/ipc_tests.rs`).

## Local AI

JARVIS talks to AI servers over HTTP. Configure them on the **Models** screen
(or in `config.toml` under `[[ai.providers]]`).

| Server | Type | Default URL | Notes |
| --- | --- | --- | --- |
| [Ollama](https://ollama.com) | Ollama | `http://127.0.0.1:11434` | preconfigured; `ollama pull llama3.2` |
| LM Studio (local server) | OpenAI-compatible | `http://127.0.0.1:1234/v1` | preconfigured; start the server in LM Studio |
| llama.cpp `llama-server` | OpenAI-compatible | `http://127.0.0.1:8080/v1` | `llama-server -m model.gguf` |
| vLLM / LocalAI / others | OpenAI-compatible | server-specific, usually ends in `/v1` | |

- Servers on another machine work if the host is a private IP or a LAN name
  (`gpu-box`, `nas.local`). Ollama listens on localhost only by default; set
  `OLLAMA_HOST=0.0.0.0` on the server to expose it to the LAN.
- Any other host is treated as **cloud** and receives conversations only if
  *Settings → Privacy → Allow cloud providers* is on.
- API keys are not supported yet.
- Thinking models (`<think>…</think>`) are shown with a collapsible
  "Reasoning" block.
- First requests to a model can be slow while the server loads it; the idle
  timeout (Settings → AI, default 120 s) also bounds that wait.

### Embedded runtime

The **Runtime** screen manages a llama.cpp `llama-server` that JARVIS starts
itself:

1. *Engine*: pick CPU or Vulkan (recommended when a GPU and the Vulkan loader
   are present), press **Find build**, review tag, file, size and SHA-256,
   then **Download & install**. Or use an existing `llama-server` (any
   backend, e.g. CUDA).
2. *Models*: download a curated model or any public GGUF from Hugging Face
   (`owner/repo` + file name), or import a local `.gguf` (stays in place).
3. Press **Start**. The model appears as provider "JARVIS runtime" in Models
   and Chat; automatic routing prefers it.

Inference settings: context size, threads (default: physical cores), GPU
offload (auto/off/all/custom) and an optional model to start with JARVIS.
JARVIS refuses to start a model that does not fit into free memory.

Developers can run the supervisor tests without llama.cpp: they start the
test binary itself as a fake `llama-server`
(`cargo test -p jarvis-runtime`).

## Where JARVIS stores data

| What | Windows | Linux |
| --- | --- | --- |
| Settings (`config.toml`) | `%APPDATA%\ai.jarvis.desktop\` | `~/.config/ai.jarvis.desktop/` |
| Local data | `%LOCALAPPDATA%\ai.jarvis.desktop\` | `~/.local/share/ai.jarvis.desktop/` |
| Runtime engine, models | `…\runtime\`, `…\models\` in local data | `runtime/`, `models/` in local data |
| Logs (`jarvis.YYYY-MM-DD.log`) | `%LOCALAPPDATA%\ai.jarvis.desktop\logs\` | `~/.local/share/ai.jarvis.desktop/logs/` |

The exact paths are shown on the Settings screen.

## Debugging

- Verbose logs: `RUST_LOG=debug npm run tauri dev` (PowerShell:
  `$env:RUST_LOG='debug'; npm run tauri dev`). `RUST_LOG` overrides the log
  level from settings.
- WebView DevTools: right-click → *Inspect* in debug builds.
- If the settings file is broken, JARVIS starts with defaults, shows a
  degraded *Configuration* component, and backs the file up when you save.

## Troubleshooting

- **`STATUS_ENTRYPOINT_NOT_FOUND` when running Rust tests on Windows**: the
  test binaries need the Common Controls v6 manifest. `build.rs` embeds it via
  linker flags for all targets; if you changed `build.rs`, make sure
  `embed_windows_manifest_via_linker` still runs.
- **npm warns about install scripts** (`esbuild`, `lmdb`, …): npm 11 does not
  run dependency install scripts unless approved. The build does not need
  them (esbuild ships platform binaries as optional dependencies; `lmdb` and
  `msgpackr-extract` are optional caches).
- **Blank window on Linux under some GPU drivers**: try
  `WEBKIT_DISABLE_DMABUF_RENDERER=1 npm run tauri dev`.

## Verified environments

| Check | Windows 11 (local + CI `windows-latest`) | Linux (CI `ubuntu-24.04`) |
| --- | --- | --- |
| `cargo fmt --check`, `clippy -D warnings`, `cargo test` | ✔ | ✔ |
| Angular tests + production build | ✔ | ✔ |
| `tauri build --no-bundle` | ✔ | ✔ |
| App launch and UI check (CDP-driven, incl. mock Ollama server) | ✔ | not yet verified |
| Runtime supervisor with real processes (fake `llama-server`) | ✔ | CI only |
| Runtime screen: hardware detection, llama.cpp release lookup (no install) | ✔ | not yet verified |
| Real llama.cpp build + GGUF model end to end | not yet verified | not yet verified |
| Real Ollama / LM Studio | not installed on the dev machine | not verified |

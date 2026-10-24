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
  libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev libasound2-dev
```

Fedora: `sudo dnf install webkit2gtk4.1-devel openssl-devel curl wget file
libappindicator-gtk3-devel librsvg2-devel libxdo-devel alsa-lib-devel` and the
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

# frontend formatting (Prettier; .prettierignore lists what it skips)
npm run format:check   # or: npm run format

# frontend lint: ESLint with angular-eslint (TypeScript + templates, incl.
# accessibility; OnPush and standalone enforced) and Stylelint for SCSS
npm run lint
npm run lint:styles    # or: npx stylelint "src/**/*.scss" --fix

# release binary without installers
npm run tauri -- build --no-bundle

# installers (NSIS/MSI on Windows; deb/rpm/AppImage on Linux). The first
# Windows run downloads the NSIS and WiX tools; AppImage downloads linuxdeploy.
npm run tauri build

# smoke test of a built binary (fresh temporary profile; see docs/RELEASE.md)
node e2e/smoke.mjs ../../target/release/jarvis.exe --plugin ../../examples/plugins/text-tools/server.mjs
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
- OpenAI, Anthropic and Gemini can be selected on the Models screen. Enter an
  API key, test the connection, then save. Keys are stored in Windows Credential
  Manager or Linux Secret Service, never in `config.toml`. On Linux a running,
  unlocked Secret Service (for example GNOME Keyring or KWallet) is required.
- Leave the key blank when editing to retain it, or use **Delete saved API key**.
  Key changes apply to new requests immediately; in-flight requests retain their
  existing provider. Connection tests contact the selected endpoint even when
  cloud chat is disabled, but do not send conversation content.
- Thinking models (`<think>…</think>`) are shown with a collapsible
  "Reasoning" block.
- First requests to a model can be slow while the server loads it; the idle
  timeout (Settings → AI, default 120 s) also bounds that wait.

### Embedded runtime

The **Runtime** screen manages a llama.cpp `llama-server` that JARVIS starts
itself:

1. *Engine*: pick CPU or Vulkan (recommended when the Vulkan loader and a GPU
   with at least 4 GB of dedicated memory are present), press **Find build**,
   review tag, file, size and SHA-256, then **Download & install**. Or use an
   existing `llama-server` (any backend, e.g. CUDA). If replies look garbled
   with Vulkan (seen on a Radeon Vega 8), set *GPU offload* to *Off*.
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

### Memory

Conversations, facts, projects and the action history live in
`memory/jarvis.db` (SQLite) in the local data folder; the **Memory** screen
shows the path, size and contents.

- **Semantic search** needs an embedding model. Without Ollama: download
  *BGE-M3 embeddings* on the **Runtime** screen and press *Start embeddings*
  (it runs in its own llama.cpp server next to the chat model; *Embedding
  model at start-up* starts it with JARVIS). With Ollama:
  `ollama pull nomic-embed-text` (or another embedding model). JARVIS picks
  a local model that reports embeddings automatically (the runtime's first),
  or choose one under *Memory → Data & settings*. Without one, search and
  recall are text-only.
- **Context size:** for the embedded runtime JARVIS plans with the context
  size it started llama.cpp with; for Ollama with at most 4 096 tokens
  (Ollama's default). *Context limit* lowers it further.
- **Changing the schema:** add `crates/jarvis-storage/migrations/NNNN_name.sql`
  and append it to `MIGRATIONS` in `migrations.rs`. Never edit a released
  migration: databases record a checksum of every applied migration and
  refuse to open if one changed.
- Tests use in-memory or temporary databases (`cargo test -p jarvis-memory`,
  `cargo test -p jarvis-core --test memory_turns`).

### Agents and tools

1. Open **Agents → Tools & permissions** and add a workspace folder (the
   folder dialog runs in Rust). File tools see nothing outside these
   folders; JARVIS's own settings, data and logs stay off limits.
2. Turn on the tools you want. Read-only tools are on by default; changes
   and programs ask for approval in a dialog. To let `run_command` /
   `launch_program` work, list the programs (plain names, e.g. `git`).
3. Start a task on the **Tasks** tab. Small local models (3B) follow the
   JSON protocol imperfectly; 7B+ instruction models work noticeably better.

Settings live in `config.toml` under `[tools]`, `[agents]` and
`privacy.allowCloudAgents`. Tasks and the audit log are tables in
`memory/jarvis.db` (migration `0002_agents.sql`).

Tests: `cargo test -p jarvis-security -p jarvis-tools -p jarvis-agent`
(real files and processes in temporary folders, a scripted model for the
orchestrator) and `cargo test -p jarvis-core --test agent_tasks` (settings,
privacy, approvals, protected folders, restarts).

`npm run test:ci` also runs `agent-ipc.integration.spec.ts`: the real Angular
components, `AgentsStore`, `JarvisApi`, `TauriBridge` and Tauri JavaScript SDK
exercise chat task creation, event delivery, allow/deny decisions and recovery
after an IPC failure. Only the native transport is replaced with Tauri's
`mockIPC`; command names and payloads are asserted. This is not a native WebView
or ACL test, and the simulated result does not prove that a filesystem tool ran.
The Rust `agent_tasks` suite separately covers actual policy and tool execution.

For a native smoke check, launch `npm run tauri dev`, select a working model,
and use a dedicated temporary workspace with a disposable file. Start a task
from Chat that requests a file change; verify that its task link opens the same
journal, deny the action and check the file is unchanged. Repeat with approval
and verify the result, journal and audit log. Connection recovery additionally
requires controlled transport fault injection; an ordinary successful launch
does not validate that path. Do not mark native recovery verified from the
JavaScript transport tests alone.

**Adding a tool:** implement `jarvis_tools::Tool` — `prepare` validates the
input with `deny_unknown_fields`, resolves paths through the scope and
returns an `ActionRequest` with honest capabilities and risk, without side
effects; `run` does the work, respects `ctx.cancel` and the limits — and
register it in `ToolRegistry::builtin`. The policy, approvals, audit log and
UI pick it up automatically.

### Plugins and MCP

- `jarvis-plugins` contains the MCP client (`mcp.rs`), server processes
  (`process.rs`), the tool adapter (`tool.rs`), manifests, the store
  (`<config>/plugins.json`) and the `PluginManager`.
- `cargo test -p jarvis-plugins` runs unit tests and `tests/servers.rs`:
  that test binary has its own `main` (`harness = false`) because it
  re-runs itself as a fake MCP server (`JARVIS_FAKE_MCP=<mode>`) for modern,
  legacy, silent, oversized and crashing servers, and drives the manager
  end to end (review, install, policy, risk reset, restarts, fingerprint
  changes, keychain secrets).
- Try a real plugin: Plugins → **Choose a plugin folder…** →
  `examples/plugins/text-tools` (needs Node.js). Or **Add an MCP server**
  with program `node` and the absolute path of `server.mjs` as argument.
- Third-party MCP servers (for example `npx -y @modelcontextprotocol/server-…`)
  download code when they start; review them like any other program.

### Voice and vision

- `jarvis-voice` runs two external engines as child processes:
  `whisper-server` (whisper.cpp) for recognition and Piper for speech.
  Settings → Voice installs them, a Whisper model and voices into
  `<data>/voice/<package id>/`; every file is pinned with its SHA-256 in
  `crates/jarvis-voice/src/catalog.rs`. Before each start the program's
  and its libraries' SHA-256 are checked against the install record
  (`jarvis-package.json`).
- Tests never need audio hardware or engines: `jarvis_voice::testing`
  (feature `test-support`) fakes the microphone, speakers and engines, and
  `tests/engines.rs` re-runs the test binary as a fake `whisper-server`
  and a fake Piper.
- Linux needs `libasound2-dev` to build (cpal → ALSA). Under WSLg, audio
  goes through PulseAudio; ALSA programs reach it with the
  `libasound2-plugins` package (`pulse` device).
- `jarvis-vision` captures with xcap on Windows, x11rb on X11 and the
  xdg-desktop-portal Screenshot dialog on Wayland. Without a portal (for
  example WSLg) capture reports itself as unavailable. Image decoding and
  resizing are optimized even in dev builds (`[profile.dev.package]` in
  the root `Cargo.toml`), otherwise a 4K screenshot takes seconds.
- Local vision models need a projector (`mmproj`) next to the model:
  catalog vision models download it automatically; for other models use
  "Add vision" on the Runtime screen.

## Where JARVIS stores data

| What | Windows | Linux |
| --- | --- | --- |
| Settings (`config.toml`) | `%APPDATA%\ai.jarvis.desktop\` | `~/.config/ai.jarvis.desktop/` |
| Local data | `%LOCALAPPDATA%\ai.jarvis.desktop\` | `~/.local/share/ai.jarvis.desktop/` |
| Runtime engine, models | `…\runtime\`, `…\models\` in local data | `runtime/`, `models/` in local data |
| Memory database (also agent tasks, audit log) | `…\memory\jarvis.db` in local data | `memory/jarvis.db` in local data |
| Logs (`jarvis.YYYY-MM-DD.log`) | `%LOCALAPPDATA%\ai.jarvis.desktop\logs\` | `~/.local/share/ai.jarvis.desktop/logs/` |

The exact paths are shown on the Settings screen. `JARVIS_UPDATE_ENDPOINT`
replaces the update feed URL for tests (release builds accept HTTPS only;
the signature check always applies — see docs/RELEASE.md). `JARVIS_PROFILE_DIR`
(an absolute path) keeps settings, data and logs in one folder instead —
used by the release smoke test and for portable use. The WebView cache
(`EBWebView`) stays in the local data folder; JARVIS keeps no state there.

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
- **Blank window on Linux under some GPU drivers, and always under WSLg**
  (WSL on Windows 11): WebKitGTK cannot use DMA-BUF buffers there
  (`libEGL warning: failed to get driver name`, `ZINK: failed to choose
  pdev`). Start JARVIS with `WEBKIT_DISABLE_DMABUF_RENDERER=1`, e.g.
  `WEBKIT_DISABLE_DMABUF_RENDERER=1 npm run tauri dev`.
- **Running the Linux build from WSL**: build in WSL with its own target
  folder and the UI built on Windows, so the shared `node_modules` is not
  replaced with Linux binaries:
  `npm run build` (Windows), then in WSL
  `CARGO_TARGET_DIR=~/jarvis-target cargo build -p jarvis-desktop --features tauri/custom-protocol`
  and `WEBKIT_DISABLE_DMABUF_RENDERER=1 ~/jarvis-target/debug/jarvis-desktop`.
  Models on the Windows disk can be imported in place from `/mnt/c/...`
  (loading over 9P is slower).

## Verified environments

| Check | Windows 11 (local + CI `windows-latest`) | Linux (CI `ubuntu-24.04`; WSL Ubuntu 24.04 + WSLg, Wayland) |
| --- | --- | --- |
| `cargo fmt --check`, `clippy -D warnings`, `cargo test` | ✔ | ✔ |
| Angular tests + production build | ✔ | ✔ |
| `tauri build --no-bundle` | ✔ | ✔ |
| App launch and UI check (CDP-driven, incl. mock Ollama server) | ✔ | ✔ WSLg (needs `WEBKIT_DISABLE_DMABUF_RENDERER=1`); not on a physical X11/Wayland desktop |
| Runtime supervisor with real processes (fake `llama-server`) | ✔ | CI only |
| Runtime screen: hardware detection, llama.cpp release lookup (no install) | ✔ | ✔ WSL |
| Real llama.cpp build + Qwen2.5 3B: install, start, chat, stop | ✔ b11739 Vulkan, CPU offload; Vulkan on Radeon Vega 8 gives garbled output | ✔ WSL: b11782 CPU build installed through the app, model imported from `/mnt/c`, streamed chat saved and kept after restart |
| Agents with the real model: tools, policy, verifier, journal, audit | ✔ (approvals allowed and denied, injected delete held) | ✔ WSL: read-only tasks, workspace folder from the GTK dialog, tasks kept after restart; approval dialog not exercised on Linux yet |
| Real Ollama / LM Studio | not installed on the dev machine | not verified |
| Voice: packages installed through Settings → Voice (whisper.cpp b5454, Whisper Small, Piper, uk voice), SHA-256 verified | ✔ | not verified live |
| Voice: speech through the speakers, reply read aloud while it streams, microphone capture (WASAPI), wake word mode on/off with indicator | ✔ (recognition checked on Piper-generated speech; the laptop mic array cancels its own speakers, so the acoustic loop is no test; wake word detection not tried with a live voice) | not verified live |
| Plugins: example MCP server (Node.js) added through the Plugins screen, protocol 2026-07-28 negotiated, an agent with Qwen2.5 3B calling `text-tools__word_count` after approval (correct answer, audit entries), changed script stopped until reviewed again, disable and remove (process gone, `plugins.json` empty) | ✔ (the folder dialog path is covered by tests only) | CI only |
| Release smoke test (`e2e/smoke.mjs`) on the release binary: every screen, echo chat turn, MCP plugin, idle renderer, clean shutdown | ✔ (17/17; UI ready 1.7–2.3 s after start, `jarvis.exe` 45 MB, WebView2 processes about 420 MB in total) | ✔ WSL release build: start-up, still running after 10 s, exit (3/3) |
| Installers | NSIS/MSI not built locally (the first build downloads NSIS and WiX); built by the Release workflow | ✔ WSL: deb and rpm (11 MB each); depends on `libasound2t64` or `libasound2`, `libwebkit2gtk-4.1-0`, `libgtk-3-0` |
| Signed updates (`e2e/update-test.mjs`, debug build, real key) | ✔ 5/5: version mismatch refused, changed file refused, signed installer started with `/P /UPDATE /R`, core stopped first | WSL: signed deb and rpm built (`.sig` bound to version 0.1.0); installing through the updater not verified |
| Idle CPU (release, dashboard, 8 logical cores) | ✔ 2.6 % focused, 1.1 % in the background (was 7.5 %) | not measured |
| Vision: screenshot and window capture, Qwen3-VL 2B + mmproj answering about the screen | ✔ (b11739 CPU, 70–85 s per screenshot question) | unit tests only (X11, portal) |

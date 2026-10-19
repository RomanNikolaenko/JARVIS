# Security

JARVIS will eventually run local models, execute tools and access files,
microphone and screen. Security is therefore part of the architecture from the
first phase. This document separates what is **implemented** from what is
**planned** — nothing listed as planned should be relied on yet.

## Threat model (summary)

Untrusted inputs: model output, web pages, files and documents, terminal
output, plugins and MCP servers, anything rendered in the WebView. Assets:
user files, conversations and memory, API keys and tokens, the ability to run
programs, microphone/camera/screen.

Key risks: prompt injection that leads to tool misuse, command injection,
exfiltration of local data to cloud providers, compromise of the WebView
leading to privileged IPC calls, secrets leaking into logs or files.

## Implemented controls (Phase 1)

| Control | Where |
| --- | --- |
| Deny-by-default IPC: every app command must be granted by a capability; the main window gets exactly the commands it uses and only `core:event:allow-listen/unlisten` from core permissions. Verified by IPC tests (calls from a window without a capability are rejected). | `src-tauri/build.rs`, `capabilities/main-window.json`, `src/ipc_tests.rs` |
| Strict Content Security Policy: no remote scripts, no inline scripts, no `eval`, `object-src 'none'`, `form-action 'none'`, IPC-only `connect-src`. `style-src` allows `'unsafe-inline'` because Angular injects component styles at runtime. | `tauri.conf.json` |
| `freezePrototype` and no global Tauri object (`withGlobalTauri: false`). | `tauri.conf.json` |
| UI has no file system, shell, HTTP or OS access; it only calls typed commands. No Tauri plugins with broad permissions are installed. | architecture |
| Input validation and limits in the core (message count/size, temperature, timeouts, concurrency, UI error size and rate). | `jarvis-core` |
| Privacy policy enforced in the core: cloud AI providers are blocked unless the user enables them; blocked attempts are logged as security events. | `jarvis-core::chat` |
| No `unsafe` code (`unsafe_code = "forbid"`), no `unwrap`/`expect` outside tests (clippy). | `Cargo.toml` |
| Settings writes are atomic; invalid files are backed up, never silently overwritten. | `jarvis-core::config` |
| AI servers are classified by URL: loopback = on device, private/LAN = local network, anything else = cloud (blocked unless allowed). Provider URLs with credentials, queries or non-HTTP schemes are rejected. | `jarvis-ai::config` |
| No implicit proxies for AI traffic; connection timeouts; streaming line size cap (4 MiB). | `jarvis-ai::http` |
| Model output is untrusted: rendered Markdown shows raw HTML as text, links are inert (no navigation), images are not loaded; Angular sanitizes again and the CSP blocks scripts and remote images. | `shared/markdown` |
| The UI never contacts AI servers itself; all network access goes through the core. System fonts only, no CDN assets. | UI, CSP |
| Embedded runtime: llama.cpp builds only from the official GitHub repository, installed after explicit approval of the exact release, re-verified by tag, SHA-256 checked, extracted without path traversal or escaping links; executable hash checked before every start. | `jarvis-runtime::installer` |
| The local model server listens on 127.0.0.1 with a random per-launch API key (checked after start-up), runs without a console window, is stopped on exit and cleaned up after crashes only if the pid still runs the recorded executable. | `jarvis-runtime` |
| Model files: GGUF headers parsed with strict bounds; downloads only from Hugging Face over HTTPS, pinned to a commit and SHA-256 verified; JARVIS deletes a model file only on request and only inside its own models folder (checked on canonical paths, not trusted from the index); files imported from elsewhere are never deleted. | `jarvis-runtime::gguf`, `::download`, `::library` |
| File pickers run in Rust; the WebView gets no file system or dialog permissions and cannot pass arbitrary paths. | `runtime_commands.rs` |
| Microphone, camera and screen are not accessed at all. | — |

Known trade-off: the *Test connection* action on the Models screen lets the
main window make the core send a `GET` request to a URL the user typed (no
conversation data). This is intended; the command is granted to the main
window only.

## Planned controls (not implemented yet)

- Security Policy Engine with capability-based tool permissions, allowlists,
  scoped file access and per-action user approval for risky operations
  (Phase 5).
- Tamper-evident audit log of tool executions (Phase 5).
- Secrets (API keys, tokens) in the OS keychain (Windows Credential Manager,
  Secret Service on Linux); never in `config.toml`, logs or the UI
  (remaining Phase 2 work, required before cloud providers).
- Separation of trusted instructions and untrusted content in prompts;
  prompt-injection mitigations for tool use (Phase 5).
- Command execution without a shell (argument vectors), working-directory and
  timeout limits (Phase 5).
- Visible indicators and explicit consent for microphone, camera and screen
  capture (Phase 6).
- Signed installers and updates (Phase 8).

Process isolation/sandboxing of tools is **not** implemented; do not assume
it exists.

## Reporting a vulnerability

Please do not open public issues for security problems. Contact the
maintainers privately (a dedicated address will be published before the first
release) with steps to reproduce and the affected version.

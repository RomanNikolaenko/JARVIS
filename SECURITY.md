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

## Implemented controls

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
| Embedded runtime: llama.cpp builds only from the official GitHub repository, installed after explicit approval of the exact release, re-verified by tag, SHA-256 checked, extracted without path traversal or escaping links; hashes of the executable and of every shared library next to it checked before every start. | `jarvis-runtime::installer` |
| The local model servers (chat and embeddings, each its own process) listen on 127.0.0.1 with a random per-launch API key passed in the environment, not on the command line (enforcement checked after start-up), run without a console window, are stopped on exit and cleaned up after crashes only if the pid still runs the recorded executable. | `jarvis-runtime` |
| Model files: GGUF headers parsed with strict bounds; downloads only from Hugging Face over HTTPS, pinned to a commit and SHA-256 verified; JARVIS deletes a model file only on request and only inside its own models folder (checked on canonical paths, not trusted from the index); files imported from elsewhere are never deleted. | `jarvis-runtime::gguf`, `::download`, `::library` |
| File pickers run in Rust; the WebView gets no file system or dialog permissions and cannot pass arbitrary paths. | `runtime_commands.rs` |
| Memory stays on this computer in one SQLite file (`memory/jarvis.db`, mode 0600 in a 0700 directory on Unix). Deleted items are overwritten (`secure_delete`), deleting everything also compacts the file. The database is **not encrypted at rest**. | `jarvis-storage` |
| Schema migrations are versioned and checksummed; a database from a newer build or with a changed migration is refused, a backup is written before migrating existing data, a damaged file is reported and never replaced. | `jarvis-storage::migrations` |
| Saving can be turned off (nothing is written; chats are temporary); temporary chats are never written. Saved memory (facts, excerpts of other conversations, texts to embed) is sent only to on-device/LAN models unless the user allows sharing with cloud models. Memory is given to the model as information, not instructions. | `jarvis-core::memory` |
| Deleting all memory requires a fixed confirmation phrase in the IPC call, sent only after the user typed a confirmation word; conversations and facts are deleted individually with inline confirmation. | `memory_commands.rs`, Memory screen |
| Memory import accepts only the JARVIS export format and version, checks sizes (≤ 256 MB, item counts) and field lengths, never overwrites existing items and runs in one transaction. Export and import files are chosen in native dialogs in Rust. | `jarvis-memory::export`, `memory_commands.rs` |
| The action history stores what JARVIS did (events), never message text, and is pruned after the retention period. | `jarvis-core::memory` |
| **Security Policy Engine** for every tool call: a tool describes the call (capabilities, risk, paths, program, URL) before anything happens; tools that are off, paths outside the workspace folders or inside JARVIS's own settings/data/log folders (canonical paths: `..`, symlinks and junctions cannot escape), programs not on the allowlist, and non-public URLs are refused. | `jarvis-security::policy`, `::scope` |
| **Approvals**: high-risk actions (overwrite or delete files, run or stop programs) always ask; medium-risk actions ask unless the user allowed them; files that may hold secrets (`.ssh`, `.env`, keys, `id_rsa`, …) always ask. The dialog shows the exact input, focuses "Deny", expires (default 5 min) and is withdrawn on cancel or exit. | `ApprovalBroker`, approval dialog |
| **Tamper-evident audit log** of every decision and result (SHA-256 chain, integrity check in the UI); entries outlive deleted tasks. | `jarvis-security::audit` |
| **Commands without a shell**: plain program names from the user's allowlist, resolved only from absolute `PATH` entries, argument vectors (no metacharacters interpreted), stdin closed, time limit, output limit, killed on cancel; the local model's API key variables are removed from the environment. | `jarvis-tools::command` |
| **git** runs without hooks, fsmonitor, pager, external diff/textconv or credential helpers, read-only subcommands only. | `jarvis-tools::git` |
| **Web fetching** is off by default; only public `http(s)` URLs without credentials; every address the host resolves to is checked (no loopback, private, link-local, CGNAT, multicast…), the connection is pinned to the checked address, redirects are re-checked (≤ 5), no proxies, text only, ≤ 2 MiB. | `jarvis-tools::web`, `jarvis-security::policy` |
| **Prompt injection**: tool output is passed to the model as framed data that cannot close its frame, and the model is told never to follow instructions in it; whatever the model then asks for still goes through the policy and approvals. | `jarvis-agent::protocol` |
| **Agent bounds**: steps, wall-clock time, invalid replies, consecutive failures, model errors and concurrent tasks are limited; no action is retried automatically; running tasks pause on exit and resume from the journal without re-running actions. | `jarvis-agent` |
| **Agents and cloud models**: a cloud model can be used by agents only with a separate permission (`privacy.allowCloudAgents`) on top of `allowCloudProviders`, because agents send file contents and program output to the model. | `jarvis-core::agents` |
| Workspace folders are added through a native folder dialog in Rust. | `agent_commands.rs` |
| **Microphone** opens only for push-to-talk or in wake word mode (off by default); an indicator is shown on every screen while it is open, and opening/closing is logged. Speech is recognized on the computer. | `jarvis-voice`, `jarvis-core::voice` |
| **Screen, windows and camera** are captured only when the user asks; captures are shown before sending, kept in memory for at most 30 minutes, used once, and reach cloud models only with `privacy.shareCapturesWithCloud`. | `jarvis-vision`, `jarvis-core::vision` |
| **Voice engines** (whisper.cpp, Piper) are pinned downloads with SHA-256, installed after confirmation; program and library hashes are checked before every start. | `jarvis-voice::catalog` |
| **Plugins (MCP servers)** start only after the user reviewed the exact program, arguments, folder, variables and a fingerprint of the files; the fingerprint is checked before every start, so a changed plugin stops until it is reviewed again. Servers get a minimal environment and secrets from the OS keychain, never from `plugins.json`. Messages are size-limited, requests have deadlines. | `jarvis-plugins` |
| **Plugin tools** are untrusted: high risk by default (every call asks), a lowered risk resets when the tool's definition changes, the server's annotations are hints only, descriptions are clipped and marked as third-party text in the agent prompt, results are framed data, calls are never repeated automatically. | `jarvis-plugins::tool`, `jarvis-agent::protocol` |

Known trade-off: the *Test connection* action on the Models screen lets the
main window make the core send a `GET` request to a URL the user typed (no
conversation data). This is intended; the command is granted to the main
window only.

## Provider credentials

API keys are stored in Windows Credential Manager or Linux Secret Service,
not in `config.toml`. The UI accepts a new key but cannot read back saved keys.
Changing or deleting a key rebuilds providers for subsequent requests;
already-running requests retain their original provider. A locked or unavailable
vault is reported as an error. Connection tests contact the selected endpoint
without sending conversation content, including when cloud chat is disabled.

The latest review: [docs/security-review-2026-10.md](docs/security-review-2026-10.md).

## Planned controls (not implemented yet)

- Sandboxing of plugin processes (AppContainer / bubblewrap / Landlock).
- Encryption of the memory database at rest with a key in the OS keychain.
- Signed installers and signed automatic updates (need an Authenticode
  certificate and an update signing key; until then releases ship with
  SHA-256 checksums).
- A RustSec advisory scan of the Rust dependencies in CI.

Process isolation/sandboxing of tools and plugins is **not** implemented;
do not assume it exists. A plugin process can do anything the user can. Tools run with the user's rights: the policy limits what they are
asked to do (folders, programs, URLs, approvals), it does not contain a
program once the user allowed and approved it. Prompt-injection mitigations
reduce the risk; they do not remove it — approvals are the safeguard for
anything that changes data.

## Reporting a vulnerability

Please do not open public issues for security problems. Contact the
maintainers privately (a dedicated address will be published before the first
release) with steps to reproduce and the affected version.

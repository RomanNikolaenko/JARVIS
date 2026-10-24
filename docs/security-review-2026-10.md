# Security review — October 2026 (Phase 8)

Scope: the desktop app as of Phase 7 (core, IPC, UI, runtime, voice, vision,
agents, plugins). Method: reading the IPC surface, permissions, CSP, every
place that starts programs, downloads files or handles secrets, plus
dependency audits. This is a review by the development team, not an
external audit.

## Trust boundaries

| Boundary | Treated as | Main controls |
| --- | --- | --- |
| Model output, tool results, web pages, files | untrusted data | framed `<tool_result>` blocks, Security Policy Engine, approvals, audit log |
| Plugins (MCP servers) and their tool descriptions | untrusted code and text | review + fingerprint before every start, high risk by default, no sandbox (documented) |
| Downloads (llama.cpp, models, voice engines) | untrusted until verified | HTTPS to `github.com` / `huggingface.co` only, SHA-256, releases re-checked against GitHub |
| WebView (Angular UI) | trusted UI, protected | bundled code only, strict CSP, no remote content, model output rendered as text |
| Rust core and shell | trusted | `unsafe_code = "forbid"`, typed IPC, deny-by-default permissions |

The WebView is the place where the user approves actions, changes settings
and adds plugins, so script injection there would equal full control of
JARVIS. The review therefore checked that nothing can inject script:

- `script-src 'self'`, `object-src 'none'`, `frame-ancestors 'none'`,
  `form-action 'none'`; `withGlobalTauri: false`; `freezePrototype: true`.
- Model output goes through the Markdown renderer, which escapes raw HTML,
  renders links as inert text and images as their alt text; Angular
  sanitizes the result again. No `bypassSecurityTrust*`, `eval`,
  `new Function` or `document.write` in the UI.
- No browser storage (`localStorage`, IndexedDB) holds state.
- `style-src` allows inline styles (Angular component styles); this does not
  allow scripts.

## IPC surface

- 91 commands, all granted only to the `main` window; no plugin permission
  (dialog, fs, shell) is granted to JavaScript — file and folder dialogs
  run in Rust, so the WebView never passes paths.
- Commands that accept URLs or downloads are checked in Rust: model and
  runtime downloads (host allowlist, HTTPS, SHA-256; a runtime release is
  re-fetched from GitHub instead of trusting the UI's copy), provider
  connection tests (a known, documented trade-off).
- Secrets (provider API keys, plugin secrets) travel UI → core once and are
  stored in the OS keychain; they are never sent back to the UI and not
  logged (checked: no logging of keys, tokens or passwords).

## Findings

| # | Finding | Severity | Status |
| --- | --- | --- | --- |
| 1 | The event log (also written to the log files) recorded a plugin's full command line when it was added; users sometimes pass tokens as arguments. | Low | Fixed: the event names the program and the fingerprint only. |
| 2 | Development dependencies: Vite 8.3.0 inside `@angular/build` (dev server: cross-origin script execution, file exposure) and `fast-uri` 3.1.8. Not part of the shipped app. | Medium (dev only) | Fixed: npm override to Vite 8.3.4, `fast-uri` 3.1.9; `npm audit` reports 0. |
| 3 | No installer, update or release pipeline: users would build from source or copy unsigned binaries. | Medium | Fixed: installers, a draft-release workflow with checksums, and signed updates bound to the version (ADR 0014). Authenticode signing of the installers needs a certificate (open). |
| 4 | Rust dependencies were not checked against the RustSec advisory database (needs `cargo-audit`, not installed). | Unknown | Open. |
| 5 | Plugins and agent programs run with the user's rights (no sandbox). | Accepted risk | Documented in SECURITY.md and on the Plugins screen. |

Production npm dependencies: `npm audit --omit=dev` — 0 vulnerabilities.

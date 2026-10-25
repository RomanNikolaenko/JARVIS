# Changelog

All notable changes to JARVIS. Versions follow [Semantic Versioning](https://semver.org).

## [0.1.1] — 2026-10-25

### Changed

- The Windows installer and uninstaller show the JARVIS icon instead of the
  NSIS default, and the installer moves on to the finish page by itself
  once the files are installed.

## [0.1.0] — 2026-10-25

The first version: a local-first desktop assistant for Windows 10/11 and
Linux (X11 and Wayland).

### Added

- **Foundation**: Rust workspace, Tauri 2 shell, Angular 22 UI with a dark
  HUD design; typed IPC with generated TypeScript types and deny-by-default
  permissions; health checks, system event log, rotating logs; CI on
  Windows and Linux.
- **AI providers**: Ollama and OpenAI-compatible servers (auto-detected),
  OpenAI, Anthropic and Gemini with API keys in the OS keychain; model
  routing by task; streamed chat with stop. Checked live against Ollama
  0.40.11 and the llama.cpp server (chat, Stop, embeddings for memory);
  token usage is requested from OpenAI-compatible servers.
- **Embedded runtime**: official llama.cpp builds installed after approval,
  verified GGUF downloads, a supervised server with memory checks, an
  embedding server for semantic search.
- **Memory**: conversations, facts, projects and an action history in a
  local SQLite database; search by words and meaning; export and import;
  context budgets with summaries.
- **Agents and tools**: multi-step tasks with file, program, system, web
  and git tools behind a Security Policy Engine, approvals and a
  tamper-evident audit log.
- **Voice and vision**: speech recognition (whisper.cpp) and speech (Piper)
  on the computer, wake word «Джарвіс», a microphone indicator; screenshots,
  windows, image files and camera photos for vision models.
- **Plugins and MCP**: plugins are MCP servers (protocol 2026-07-28 and the
  older handshake) reviewed and fingerprinted before every start; their
  tools go through the same policy; example plugin `text-tools`.
- **Window**: opens maximized; a restored window is never larger than the
  work area of its monitor (the screen without the taskbar).
- **Release**: installers (NSIS, MSI, deb, rpm, AppImage) for x64 and
  ARM64, a draft-release workflow with checksums and a "Which file do I
  need?" table; every installer except the rpm is installed, smoke tested
  and uninstalled on clean machines in CI. The Windows ARM64 installer
  refuses other processors and names the right file.
- **Updates**: signed automatic updates from GitHub Releases; an update
  button in the top bar shows the running version, offers a new one,
  downloads, verifies and installs it on one click with progress, and
  offers a retry after a failed download.
- **Developer tools**: `provider-probe` (real model servers through
  JARVIS's provider code) and `mcp-probe` (MCP servers) examples.
- **License**: MIT OR Apache-2.0; the installers include the licenses of
  all shipped third-party crates and packages (`THIRD-PARTY-NOTICES.txt`).

### Not included

- Cloud sign-in or sync: all data stays on the computer (ADR 0013).
- Authenticode-signed installers (need a certificate).

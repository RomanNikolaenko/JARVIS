# Changelog

All notable changes to JARVIS. Versions follow [Semantic Versioning](https://semver.org).

## [0.1.0] — unreleased

The first version: a local-first desktop assistant for Windows 10/11 and
Linux (X11 and Wayland).

### Added

- **Foundation**: Rust workspace, Tauri 2 shell, Angular 22 UI with a dark
  HUD design; typed IPC with generated TypeScript types and deny-by-default
  permissions; health checks, system event log, rotating logs; CI on
  Windows and Linux.
- **AI providers**: Ollama and OpenAI-compatible servers (auto-detected),
  OpenAI, Anthropic and Gemini with API keys in the OS keychain; model
  routing by task; streamed chat with stop.
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
- **Release**: installers (NSIS, MSI, deb, rpm, AppImage), a draft-release
  workflow with checksums, and a release smoke test.

### Not included

- Cloud sign-in or sync: all data stays on the computer (ADR 0013).
- Signed installers and automatic updates (need the owner's keys).

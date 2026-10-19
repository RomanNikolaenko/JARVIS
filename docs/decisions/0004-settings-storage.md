# 0004 — Settings in a validated TOML file with atomic writes

- Status: Accepted
- Date: 2026-10-19

## Context

Settings must survive restarts, be readable and recoverable by users, and
must never be lost because of a crash or a malformed file. SQLite arrives only
in Phase 4.

## Decision

- One `config.toml` in the OS config directory
  (`%APPDATA%\ai.jarvis.desktop` on Windows, `$XDG_CONFIG_HOME/ai.jarvis.desktop`
  on Linux). Keys use the same camelCase names as the IPC contract, so one
  model serves both.
- `schemaVersion` field; files from a newer version are rejected (never
  downgraded silently). Missing fields take defaults, unknown fields are
  ignored.
- `Settings::validate()` checks ranges and cross-field rules; the same
  validation runs for files and for IPC updates.
- Writes go to `config.toml.tmp`, are `fsync`ed and renamed over the original
  (atomic on Windows and Unix). Concurrent saves are serialised.
- An invalid file is **not** overwritten at startup. JARVIS runs on defaults,
  health reports `degraded`, and the file is renamed to
  `config.toml.invalid-<unix-ms>.bak` before the next save.

## Alternatives considered

- JSON: no comments, less friendly for manual edits.
- `tauri-plugin-store`: ties settings to the UI shell and the WebView.
- SQLite now: unnecessary before Phase 4; settings may still move there later
  (with a migration) if they grow.

## Consequences

- Secrets (API keys) must **not** be stored here; they go to the OS keychain
  (Phase 2/5).
- Schema changes require bumping `SETTINGS_SCHEMA_VERSION` and a migration.

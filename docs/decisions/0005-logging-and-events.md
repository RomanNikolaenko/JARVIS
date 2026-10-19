# 0005 — Diagnostic logging vs. user-facing system events

- Status: Accepted
- Date: 2026-10-19

## Context

Developers need detailed logs; users need a readable activity log ("settings
saved", "chat cancelled", "request to a cloud provider blocked"). Mixing both
makes logs noisy for users and loses detail for developers.

## Decision

- **Diagnostic logs:** `tracing` with `tracing-subscriber`. Output to stderr and
  to daily-rotated files in the OS log directory (14 files kept) via a
  non-blocking writer. Own crates log at the configured level, dependencies at
  `warn`; the level can change at runtime (reload layer); `RUST_LOG` overrides.
  Panics are logged. The writer is flushed on exit.
- **System events:** `jarvis-events::EventBus` (broadcast channel + bounded
  history). Events have level, category and a human-readable message, are
  forwarded to the main window and mirrored into `tracing`.
- UI errors are reported through IPC (`report_ui_error`), truncated and rate
  limited, and become both a log line and a system event.
- Never log secrets or full conversation contents.

## Alternatives considered

- `tauri-plugin-log`: convenient, but couples logging setup to the shell and
  exposes log commands to the WebView.
- One stream for both: either too noisy for users or too sparse for
  debugging.

## Consequences

- The event history is in memory only; it will be persisted (episodic memory)
  in Phase 4/5.
- The future audit log for tool execution (Phase 5) is a separate,
  tamper-evident store, not the event log.

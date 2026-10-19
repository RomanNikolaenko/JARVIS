# 0002 — Typed IPC contract with generated TypeScript types and explicit permissions

- Status: Accepted
- Date: 2026-10-19

## Context

The Angular UI and the Rust core communicate through Tauri IPC. Hand-written
TypeScript mirrors of Rust types drift silently. Any script running in the
WebView (including injected content in the future) can call IPC commands, so
the IPC surface is a security boundary.

## Decision

1. **Rust is the source of truth.** DTOs derive `serde` (camelCase) and
   `ts_rs::TS` with `#[ts(export_to = "<crate>.ts")]`. A single test,
   `apps/desktop/src-tauri/src/bindings.rs`, exports the root types of the IPC
   contract (and, transitively, everything they reference) into
   `packages/shared-types/src/generated/*.ts`. Exporting from one process is
   required: ts-rs merges several types into one file only within a process,
   so per-crate `#[ts(export)]` tests overwrote each other depending on test
   order. `u64` is mapped to `number` (`TS_RS_LARGE_INT`) because all values
   are far below 2^53. CI fails when the committed bindings differ.
2. **Structured errors:** commands return `Result<T, ErrorPayload>` with a
   stable `ErrorCode`. The UI never parses messages.
3. **Explicit permissions:** all commands are listed in `build.rs`
   (`AppManifest::commands`). Tauri then denies every app command that is not
   granted by a capability. `capabilities/main-window.json` grants exactly the
   commands the main window uses, plus `core:event:allow-listen/unlisten`.
   Nothing else from `core:default` is granted.
4. **Streaming:** request-scoped `tauri::ipc::Channel`s for chat (ordered,
   no global event name collisions); global events only for broadcast data
   (system event log), emitted to the `main` window only.
5. **Thin commands:** commands only adapt types and delegate to `jarvis-core`.
6. **Single bridge:** only `TauriBridge` imports `@tauri-apps/api`.

## Alternatives considered

- `tauri-specta` / `specta`: also generates command bindings, but was a
  release candidate at decision time and couples the core to Tauri-specific
  derive macros.
- Hand-written TS types with contract tests: more code, drift detected late.
- Global events for chat streaming: ordering and routing to the right request
  become the UI's problem.

## Consequences

- Adding a command requires four edits (command fn, `generate_handler!`,
  `build.rs`, capability). The IPC tests check that every command is allowed
  for `main` and denied for windows without a capability.
- Domain crates depend on `ts-rs` (derive only, no runtime cost).

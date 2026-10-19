# 0001 — Modular monolith: Rust workspace + Tauri 2 + Angular

- Status: Accepted
- Date: 2026-10-19

## Context

JARVIS is a desktop application for Windows and Linux that will run local AI
models, control the computer through tools and keep long-term memory. It must
start fast, stay responsive during inference, be secure by default and be
developed iteratively by a small team.

## Decision

- One desktop process built with **Tauri 2** (system WebView, small binaries,
  capability-based IPC permissions).
- **Rust** for all logic, split into workspace crates with explicit public
  APIs. `jarvis-core` is the composition root of the domain layer and has no
  dependency on Tauri or Angular; the Tauri crate is an adapter on top.
- **Angular** (standalone components, signals, zoneless) for the UI. The UI has
  no direct access to the file system, secrets, OS APIs or databases.
- Versions at the time of writing: Rust 1.98 (MSRV declared 1.90 = Tauri's),
  Tauri 2.12 (Tauri 3 is still alpha), Angular 22.2 with TypeScript 6.0,
  Node 24 LTS.
- Heavy or long-running work (local inference, STT) will run in managed child
  processes (`llama-server`, …) supervised by Rust, not as separate services.

## Alternatives considered

- **Electron**: larger memory footprint and bundle; Node in the privileged
  process; no built-in permission model for IPC.
- **Microservices / local daemons from day one**: process boundaries and
  versioning overhead without a concrete need. Can be introduced later behind
  the same traits if a component needs isolation.
- **Tauri 3 (alpha)**: not stable; migration is planned once it is released.

## Consequences

- Crates can be tested without a GUI (`cargo test` covers the domain fully).
- System WebViews differ (WebView2 on Windows, WebKitGTK on Linux); UI code
  must avoid engine-specific features and Linux needs `webkit2gtk-4.1`.
- New subsystems are added as new crates when their phase starts; no empty
  placeholder crates.

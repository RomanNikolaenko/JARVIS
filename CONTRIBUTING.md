# Contributing

## Workflow

1. Create a branch from `main`.
2. Keep changes focused; update docs (ARCHITECTURE, ROADMAP, ADRs) in the same
   change when behaviour or boundaries change.
3. Open a pull request; CI must be green on Windows and Linux.

## Quality gates (run before pushing)

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace            # also regenerates packages/shared-types
cd apps/desktop
npm run format:check
npm run lint
npm run lint:styles
npm run test:ci
npm run build
```

## Rules

- **Layers:** domain crates must not depend on Tauri or Angular. The Tauri
  crate only adapts IPC to the core. Angular never talks to the OS directly.
- **Errors:** no `unwrap`/`expect` outside tests (clippy enforces it); map
  errors to `CoreError`/`ErrorCode`. Never swallow errors silently.
- **Async:** never block the Tokio runtime; use `spawn_blocking` for blocking
  work; never hold a lock across `.await`.
- **Honesty:** do not ship placeholder implementations that look functional.
  Unsupported features return explicit `unsupported` errors; planned features
  are labelled as planned in UI and docs.
- **Security:** new IPC commands need an explicit capability entry and tests;
  no secrets in code, configs or logs; untrusted input is validated in Rust.
- **Dependencies:** add only what is needed, prefer well-maintained crates,
  check compatibility (MSRV 1.90, Angular/TypeScript peer ranges).
- **Generated code:** never edit `packages/shared-types/src/generated/` by
  hand.
- **Commits:** imperative mood, explain *why* in the body when not obvious.

## Architecture decisions

Significant decisions (new dependency category, new crate, changed boundary,
security-relevant behaviour) get an ADR in `docs/decisions/`.

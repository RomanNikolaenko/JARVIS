# Architecture Decision Records

Each ADR records one decision: context, the decision, alternatives and
consequences. ADRs are immutable once accepted; a later ADR supersedes an
earlier one instead of editing it.

| # | Title | Status |
| --- | --- | --- |
| [0001](0001-modular-monolith.md) | Modular monolith: Rust workspace + Tauri 2 + Angular | Accepted |
| [0002](0002-ipc-contract.md) | Typed IPC contract with generated TypeScript types and explicit permissions | Accepted |
| [0003](0003-ai-provider-abstraction.md) | Provider-agnostic AI abstraction, streaming and cancellation | Accepted |
| [0004](0004-settings-storage.md) | Settings in a validated TOML file with atomic writes | Accepted |
| [0005](0005-logging-and-events.md) | Diagnostic logging vs. user-facing system events | Accepted |
| [0006](0006-http-ai-providers.md) | HTTP AI providers and URL-based privacy classification | Accepted |
| [0007](0007-model-routing.md) | Model routing by task category | Accepted |
| [0008](0008-embedded-llama-runtime.md) | Embedded runtime: supervised llama.cpp server | Accepted |
| [0009](0009-local-memory.md) | Local memory: SQLite storage, Memory Engine and Context Manager | Accepted |
| [0010](0010-tools-and-agents.md) | Tools and agents behind a Security Policy Engine | Accepted |
| [0011](0011-voice-and-vision.md) | Voice and vision on the user's computer | Accepted |

Template: copy an existing ADR, increment the number, keep it short.

# 0003 — Provider-agnostic AI abstraction, streaming and cancellation

- Status: Accepted
- Date: 2026-10-19

## Context

JARVIS must work with very different backends: Ollama, LM Studio, llama.cpp
server, other OpenAI-compatible servers, an embedded llama.cpp runtime and
optional cloud APIs. They differ in capabilities, latency and privacy.

## Decision

- A single `AiProvider` trait (`jarvis-ai`) with `descriptor`, `health_check`,
  `list_models`, `capabilities`, `stream_chat`, `chat` (default: collect the
  stream) and `embeddings` (default: `Unsupported`).
- **Capabilities are explicit** (`ModelCapabilities`); callers check them
  instead of assuming support.
- **Privacy is metadata:** each provider declares its `DataLocality`. The core
  enforces policy (cloud providers are blocked unless the user enables them),
  not the UI.
- **Cancellation:** streams are `BoxStream`s; dropping one must abort backend
  work (closing the HTTP connection, …). The core wraps each request in a
  `CancellationToken` that is a child of a shutdown token, so `cancel` and
  application shutdown share one mechanism.
- **Timeouts:** overall request timeout and idle timeout between chunks, both
  configurable.
- **Retries:** only for idempotent operations (`health_check`,
  `list_models`) and only for transient errors (`AiError::is_transient`). Chat
  requests and anything an agent does are never retried automatically.
- Phase 1 includes a clearly labelled diagnostic `EchoProvider` to verify the
  streaming and cancellation pipeline end to end without a model.

## Alternatives considered

- `cancel_request(id)` on every provider: duplicates bookkeeping in each
  implementation; dropping the stream is sufficient and composable.
- Returning `impl Stream` with native `async fn` in traits: not object-safe;
  the registry needs `dyn AiProvider`. `async-trait` is used until dyn-safe
  async traits are stable.
- Callback-based streaming: harder to compose with timeouts and `select!`.

## Consequences

- New providers are added without touching the UI or chat orchestration.
- Tool calling will extend the trait (or add a sibling trait) in Phase 5.
- Model routing (Phase 2+) can use descriptors, capabilities and locality.

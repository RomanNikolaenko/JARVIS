# 0007 — Model routing by task category

- Status: Accepted
- Date: 2026-10-19

## Context

Different tasks need different models (a small fast chat model, a coding
model, a vision model, an embedding model for memory). Users must be able to
choose explicitly, but JARVIS should also work without any configuration and
must respect the privacy policy.

## Decision

- Task categories: `general`, `reasoning`, `coding`, `vision`, `embeddings`,
  `toolCalling` (speech categories arrive with the voice engine). Each
  category maps to required capabilities (e.g. `vision` = chat + vision).
- Settings `ai.routing` hold an optional explicit `ModelRef { providerId,
  model }` per category. `reasoning` and `coding` inherit the explicit
  `general` choice when unset.
- **Explicit choices win.** If the chosen model is unavailable, the request
  fails with a clear error instead of silently using another model.
- **Automatic choice** ranks available models that support the category:
  real providers before the diagnostic provider, then on-device before local
  network before cloud, then the user's provider order, then the server's model
  order. Cloud models are candidates only when allowed; if only blocked cloud
  models qualify, the error says so (`permissionDenied`).
- The router is a pure function over a **model catalog**: a cached snapshot of
  all providers' models (30 s, or 5 s if any provider failed), invalidated
  when providers change and refreshable on demand.
- Chat requests without a model use the `general` route; the UI shows which
  model "Automatic" currently resolves to (`get_model_routes`).

## Alternatives considered

- A single default model: does not scale to vision/embeddings/agents.
- Cost/performance-based scoring now: no reliable data yet; locality and
  capability ordering are predictable and explainable. Scoring can be added
  as another ranking key later.

## Consequences

- Routing decisions are testable without network access.
- A stale catalog can make a just-started server invisible for up to 5 s;
  the UI offers an explicit refresh.

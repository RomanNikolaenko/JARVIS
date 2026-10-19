# 0006 — HTTP AI providers and URL-based privacy classification

- Status: Accepted
- Date: 2026-10-19

## Context

Phase 2 connects JARVIS to real models served by local or network servers:
Ollama (native API) and the many servers that implement the OpenAI API
(LM Studio, llama.cpp server, vLLM, LocalAI, …). Users may point JARVIS at a
server on another machine. Conversations must not leave the device or the
local network unless the user explicitly allows it.

## Decision

- **Two backends** implement `AiProvider` in `jarvis-ai`:
  `OllamaProvider` (`/api/chat` NDJSON streaming, `/api/tags`, `/api/show` for
  capabilities and context length, `/api/embed`) and `OpenAiCompatibleProvider`
  (`/chat/completions` SSE streaming, `/models`, `/embeddings`).
- **HTTP client:** `reqwest` 0.13 with rustls, one shared connection pool,
  connect timeout 5 s, request timeout 10 s for non-streaming calls, and **no
  proxy** (system or environment proxies are ignored so local AI traffic is
  never routed elsewhere implicitly).
- **Streaming decoders** are incremental and byte-oriented (`LineBuffer`,
  `SseParser`): they handle lines and UTF-8 sequences split across network
  chunks and cap line length (4 MiB). Dropping a stream closes the
  connection, which is how cancellation reaches the server.
- **Retries:** only idempotent requests, only once, and only after timeouts or
  `502/503/504`. Refused connections fail immediately (on Windows a refused
  local connection already takes ~2 s).
- **Capabilities:** Ollama reports them (`completion`, `vision`, `tools`,
  `embedding`); older servers fall back to a name heuristic. The OpenAI API
  has no capability discovery, so models are treated as chat models (or
  embedding-only when the id names an embedding model); vision and tool
  calling are never assumed.
- **Configuration:** `ai.providers` in settings (id, backend, display name,
  base URL, enabled). Defaults: Ollama `http://127.0.0.1:11434` and LM Studio
  `http://127.0.0.1:1234/v1`. URLs must be `http`/`https`, contain no
  credentials, query or fragment.
- **Privacy class from the URL, never from user input:** loopback and
  `localhost` → `onDevice`; private/link-local/CGNAT addresses, single-label
  hosts and `.local`/`.lan`/`.home.arpa`/`.internal` → `localNetwork`;
  everything else → `thirdPartyCloud`, which the core blocks unless
  `privacy.allowCloudProviders` is on.
- **Factory injection:** the core receives a `ProviderFactory`; tests use fake
  factories and never touch the network. Provider sets are rebuilt atomically
  when settings change; running chats keep their provider.
- **Contract tests:** both backends run the same suite against a scripted
  HTTP server (health, listing, streaming across split chunks, unknown models,
  server errors, cancellation closes the connection, embeddings).

## Alternatives considered

- Using an OpenAI SDK crate: adds a large dependency for a small API surface
  and does not cover Ollama's native API (capabilities, context length).
- Treating every configured server as local: a URL typo or a remote host
  would silently send conversations off the network.
- `wiremock`-style mock servers: cannot express split UTF-8 chunks, hanging
  responses or observe client disconnects as directly as a 200-line scripted
  server.

## Consequences

- API keys are not supported yet; servers requiring authentication fail with
  "access denied". Keys will be stored in the OS keychain (separate ADR).
- Model listing costs one request per Ollama model (`/api/show`); results are
  cached for 30 s (5 s when a provider failed) — see ADR 0007.

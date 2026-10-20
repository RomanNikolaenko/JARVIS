# 0009 — Local memory: SQLite storage, Memory Engine and Context Manager

- Status: Accepted
- Date: 2026-10-20

## Context

JARVIS must remember conversations, facts and projects between restarts,
find them again (by words and by meaning), and fit what is relevant into the
small context windows of local models (often 4 096 tokens). It must work
fully offline, keep the user's data on the machine, and let the user turn
remembering off, delete selected items or everything, and export or import
memory. Sending the whole history with every request is neither possible nor
desirable.

## Decision

- **One SQLite database** (`<data>/memory/jarvis.db`) through `rusqlite` with
  the bundled SQLite (FTS5 included), in the crate `jarvis-storage`. One
  connection is used from blocking threads (`Database::call`); WAL journal,
  foreign keys, `secure_delete` so deleted content is overwritten. On Unix
  the file is created with mode 0600 in a 0700 directory.
- **Versioned migrations** (`crates/jarvis-storage/migrations/*.sql`) are
  append-only and recorded with a SHA-256 of their SQL (line endings
  normalized). Opening fails for a database written by a newer build (no
  silent downgrade) or when an applied migration changed. Before migrating a
  database that holds data, a copy is written with `VACUUM INTO`. Each
  migration runs in one transaction. A damaged file is reported, never
  replaced: JARVIS then offers temporary chats only.
- **Five memory layers** in `jarvis-memory` (`MemoryEngine`):
  working memory (a pinned note per conversation plus its recent turns),
  conversation memory (messages and a running summary), semantic memory
  (facts, global or per project, optionally pinned), project memory
  (description and instructions) and episodic memory (actions JARVIS took,
  recorded from the event bus without message text). Temporary
  conversations live only in memory.
- **Search** combines FTS5 (BM25, `unicode61` with diacritic folding, a
  crude prefix stem for inflected languages, stop words, term-overlap
  filter) with vector similarity, fused by reciprocal rank fusion.
  Embeddings come from an AI provider whose model reports the capability
  (or that the user picks); vectors are normalized `f32` blobs per
  embedding model and searched brute force in memory. The embedded runtime
  can serve an embedding model in its own llama.cpp server (ADR 0008), so
  semantic search works without Ollama; it comes first among the providers,
  so it is chosen automatically while it runs. The indexer runs as soon as
  the set of providers changes. Switching to another embedding model drops
  the vectors of the previous one and re-indexes. Without an embedding
  model, search and recall are text-only, and the UI says so.
- **Context Manager** assembles each request inside the model's window:
  the system prompt and the new message always; then project instructions
  (≤ 15 % of the prompt budget), the pinned note (≤ 10 %), pinned and
  relevant facts plus excerpts of other conversations of the same scope
  (≤ 20 %), the running summary (≤ 15 %) and as many recent messages as fit,
  newest first. A quarter of the window (256–half) is reserved for the
  answer and 10 % for estimation error. Tokens are estimated (≈ 4 bytes of
  UTF-8 per token, at least one per 3.2 characters), so Cyrillic is slightly
  overestimated. The window comes from the running llama.cpp context for
  the embedded runtime, at most 4 096 for Ollama (its default `num_ctx`),
  otherwise the model's reported window, capped by a user setting.
- **Compaction:** when older messages no longer fit or the verbatim history
  uses more than half of the budget, the model serving the conversation
  writes a summary of the oldest uncovered messages (the last six are never
  summarized) after the reply, in the background. Reasoning blocks
  (`<think>`) are never sent back to a model.
- **The core owns conversations.** The UI sends only the new message
  (`send_message`); the core builds the context, saves the user message
  before the request and the reply (or the partial reply of a stopped
  request) after it. One reply runs at a time per conversation.
- **Privacy rules in the core:** with saving off nothing is written (new
  chats are temporary, facts and history are not recorded); saved memory —
  facts, excerpts of other conversations, texts to embed — goes only to
  on-device or LAN models unless the user enables sharing with cloud
  models; memory is presented to the model as information, not
  instructions.
- **Export/import** as a versioned JSON document without embeddings; import
  validates format, version, sizes and field lengths, never overwrites (same
  ids are skipped) and runs in one transaction. Deleting all memory needs a
  confirmation phrase in the IPC call and compacts the file afterwards.

## Alternatives considered

- `sqlx`: async and compile-time checked queries, but needs a database at
  build time or offline metadata and adds a large dependency for a
  single-user desktop database.
- `sqlite-vec` or another loadable extension: faster vector search, but
  loading native extensions needs `unsafe` and is a supply-chain risk;
  brute force is enough for tens of thousands of items and can be replaced
  behind `VectorIndex`.
- A vector database (Qdrant, LanceDB): another service or a heavy native
  dependency for a few thousand vectors.
- SQLCipher (encryption at rest): key management (OS keychain) belongs to the
  security phase; until then the database relies on the user account's
  protection, which is stated in the UI and in SECURITY.md.
- Automatic fact extraction by the model: error-prone with small models and
  a privacy concern; facts are saved explicitly ("Remember") for now.

## Consequences

- Memory works offline and survives restarts; deleting is real (overwritten
  pages, `VACUUM`).
- Semantic search needs an embedding model: the embedded runtime's
  embedding server (curated: BGE-M3, multilingual incl. Ukrainian) or a
  provider such as Ollama. Texts are embedded as they are (at most 2 000
  characters each, no query/passage prefixes), which suits BGE-M3; models
  that expect prefixes (e.g. multilingual-e5) work less well.
- Token counts are estimates, so the budget keeps a safety margin.
- Later domains (tasks, audit log) add migrations to the same database.

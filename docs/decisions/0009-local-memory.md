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
- **Similarity cuts** decide which vector hits take part in the fusion: a
  hit must reach a floor and lie within a margin (0.10) of the best hit.
  For recall the margin is measured among the hits that may be recalled
  (in scope, not pinned, from other conversations). Cosine similarities are
  not comparable across embedding models, so the cuts depend on the model
  key: BGE-M3 (from any provider) uses measured floors — search 0.52,
  recall 0.55 — and other models keep the earlier, unmeasured floors
  (search 0.35, recall 0.50) with the same margin. Recall is stricter than
  search because a small model cannot tell an unrelated fact from a
  relevant one, while the user reads the search list.
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

## Calibration of the similarity cuts (BGE-M3)

Measured on 2026-10-20 with `ggml-org/bge-m3-Q8_0-GGUF` served by
llama.cpp b11739 the way the embedded runtime serves it (CPU,
`--embedding --pooling cls`, context and batches of 2 048). Dataset:
21 personal facts (10 Ukrainian, 11 English) and 41 queries in both
languages — 24 questions with a relevant fact (14 of the 25 relevant pairs
cross-lingual), 8 one-word queries and 9 queries about nothing in memory.
Scores are dot products of the normalized vectors:

| Pairs | n | min | median | max |
| --- | --- | --- | --- | --- |
| relevant, same language, question | 11 | 0.588 | 0.668 | 0.811 |
| relevant, cross-lingual, question | 14 | 0.443 | 0.631 | 0.752 |
| relevant, one-word query | 10 | 0.400 | 0.565 | 0.720 |
| unrelated, queries with a relevant fact | 631 | 0.204 | 0.391 | 0.606 |
| best fact of a query about nothing in memory | 9 | 0.369 | 0.432 | 0.520 |

Unrelated facts reach 0.50 at the 95th percentile. Apart from one
inferential cross-lingual pair ("Can I eat peanut butter?" → "У мене
алергія на арахіс.", 0.443) the lowest relevant question scored 0.560.
The earlier floor of 0.35 therefore let almost every fact through.

The same vectors run through `MemoryEngine::search` and `prepare_context`
(facts only, 41 queries):

| Cuts | Search: unrelated facts listed | Search: relevant missed | Recall: unrelated facts added | Recall: relevant missed |
| --- | --- | --- | --- | --- |
| before (0.35 / 0.50, no margin) | 546 | 0 | 31 | 4 |
| after (0.52 / 0.55, margin 0.10) | 10, of them 4 by word match | 4 | 6, of them 2 by word match | 4 |

Queries about nothing in memory with a semantic hit: search 9 of 9 → 1
("Порадь фільм на вечір." → the guitar fact, 0.520), recall 1 → 0.
All cross-lingual questions except the peanut-butter one (13 of 14) keep
their facts in search and recall (e.g. "Ким я працюю?" → the English job fact at 0.574, while four unrelated
facts at 0.505–0.514 no longer match semantically).

What the cuts cannot fix:

- Hard negatives the model ranks next to or between relevant facts:
  "I am allergic to cats." for "Що мені не можна їсти?" (0.597, between the
  relevant 0.671 and 0.588), the dentist appointment and the rent date for
  "When is the daily meeting?" (0.606 and 0.568 against 0.609), the
  vegetarian fact for "Чим я займаюся у вільний час?" (0.551) and, in search
  only, the dog for "Чи можна мені завести кота?" (0.535).
- Four relevant pairs below 0.5 are no longer found semantically: the
  peanut-butter question (0.443), "Atlas" → the Ukrainian "Атлас" fact
  (0.486), "birthday" (0.481, ranked below the dentist appointment) and
  "робота" (0.400, ranked below unrelated facts). The old search floor
  listed them together with almost every other fact.
- Word matches are not cut: "am" is not a stop word ("Which language am I
  learning?" lists "I am allergic to cats."), "Where do I work?" finds
  "…works as a teacher" and "Ким я працюю?" finds "…працює на
  PostgreSQL 16.".

The margin of 0.10 keeps a second relevant fact (gaps of 0.083 and 0.090
were measured; 0.08 loses both) and, above the search floor, drops hits
such as the cat allergy for "What is my dog's name?" (0.544 against
0.668). Every floor from 0.49 to 0.55 loses the same four relevant pairs
and no other; 0.52 drops the cluster of unrelated facts around 0.50–0.51,
and recall additionally drops 0.52–0.55. The recall floor has little
headroom below the lowest relevant question (0.560).

Rejected cuts, on the same data:

- A margin below the best hit alone: a query about nothing in memory still
  returns its best hit, and 175 unrelated pairs stay within 0.10 of it.
- Relative to the query's background (similarity minus the median
  similarity of the query to all facts ≥ 0.12): best on all 21 facts (2
  relevant missed, 5 unrelated), but unstable for small memories. On random
  subsets it found none of the relevant facts with 1 fact and 43 % with 2,
  and with 3–16 facts it let through about 1.5–2 times as many unrelated
  facts as the recall cut.
- A z-score over the same background: z ≥ 2 lets queries about nothing in
  memory through, z ≥ 2.5 misses 9 relevant pairs.
- One global floor for every model: embedding models place unrelated texts
  at different levels, so a floor measured for BGE-M3 does not transfer.

Not measured: other embedding models (Ollama was not installed on the
measuring machine), other quantizations of BGE-M3, and conversation
messages (longer than facts). Measuring another model means adding its
floors to `SimilarityCuts::for_model`.

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
- With BGE-M3, semantic search lists few unrelated facts and finds nothing
  for queries about nothing in memory, but matches below 0.52 (search) or
  0.55 (recall) are not found by meaning, and facts the model itself
  places close to the query still appear. Other embedding models keep the
  uncalibrated floors until they are measured.
- Token counts are estimates, so the budget keeps a safety margin.
- Later domains (tasks, audit log) add migrations to the same database.

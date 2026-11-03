# Memory: facts suggested from conversations

- Date: 2026-11-03
- Status: approved design (implementation pending)
- Area: `crates/jarvis-storage` (migration), `crates/jarvis-memory`,
  `crates/jarvis-core` (memory pipeline, IPC), Memory screen, navigation

## Goals

JARVIS notices lasting things the user says about themselves in saved
conversations (name, work, family, preferences, plans) and **suggests** them
as facts. Nothing enters memory, and so nothing reaches a reply, until the
user accepts it on the Memory screen. The feature is opt-in and off by
default (ROADMAP Phase 4, "Automatic fact extraction (opt-in)").

Non-goals: saving facts without review, suggestions in the chat itself,
updating or contradicting existing facts, extraction from temporary chats,
processing old conversations in bulk, a dedicated extraction model.

## Decisions taken with the user

1. Suggestions wait for review; accepted ones become ordinary facts.
2. Review happens on the Memory screen (Facts tab) with a counter next to
   "Memory" in the navigation.
3. Extraction runs after a pause: two minutes without chat activity, one
   request for all messages since the last check.

## Design

### 1. Data (`jarvis-storage` migration `0005_fact_suggestions.sql`)

```sql
CREATE TABLE fact_suggestions (
    id                     TEXT PRIMARY KEY,           -- UUID
    content                TEXT NOT NULL,
    normalized             TEXT NOT NULL,              -- for duplicate checks
    project_id             TEXT REFERENCES projects (id) ON DELETE SET NULL,
    source_conversation_id TEXT NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
    status                 TEXT NOT NULL CHECK (status IN ('pending', 'rejected')),
    created_at             INTEGER NOT NULL
);
CREATE INDEX fact_suggestions_by_status ON fact_suggestions (status, created_at);
ALTER TABLE conversations ADD COLUMN facts_checked_until INTEGER NOT NULL DEFAULT 0;
```

- Suggestions live in their own table, not as a status of `facts`: context
  assembly, search, the embedding indexer and export read `facts` only, so
  a pending suggestion cannot leak into a reply even if a filter is missed.
- `facts_checked_until` is the `messages.id` of the last message already
  looked at; a new conversation starts at 0.
- Accepting a suggestion creates a fact with `source = 'conversation'` and
  the suggestion's conversation as `source_conversation_id`, then deletes
  the suggestion. `facts.source` has a `CHECK` constraint and an FTS index
  with triggers; a new source value would need a table rebuild for a label
  that says the same thing ("from a conversation"). The user may change the
  text, pin it or move it to a project when accepting.
- Rejecting keeps the row with `status = 'rejected'` so the same suggestion
  is not made again. Deleting a conversation deletes its suggestions of
  both kinds (they are derived from its text). `MemoryEngine::clear`
  ("Delete everything") removes all suggestions. Export and import ignore
  them.
- `normalized`: lowercase, whitespace collapsed, trailing punctuation
  removed (`text::normalize_fact`).

### 2. When (`jarvis-core::memory`)

- After a reply of a saved conversation is stored and
  `memory.suggestFacts` is on, the conversation is marked due and a single
  scheduler (one Tokio task per core) waits until **120 s have passed
  since the last chat activity** in any conversation: every new turn,
  streamed reply or summary request postpones it.
- When the pause is reached, the scheduler processes due conversations one
  at a time, most recent first. A turn that starts meanwhile does not wait
  for it; the next due conversation waits for the next pause.
- Not processed: temporary conversations, any conversation while
  `memory.enabled` is off, conversations with no new user messages since
  `facts_checked_until`.
- Shutdown cancels a running request; the cursor stays where it was.

### 3. The request

- Model: the provider and model of the conversation's latest reply (as
  summaries use), through `AiProvider::chat`, temperature 0.2, at most 400
  output tokens, the chat request timeout.
- Input: the messages after `facts_checked_until`, bounded to the last
  3 000 estimated tokens; older unchecked messages beyond that are skipped
  for good (the cursor passes them). User messages are marked as the source; JARVIS's
  replies are included only as context, so the model's own guesses do not
  become facts about the user.
- Instructions (system prompt, English, answer in the user's language):
  extract at most 5 lasting facts **the user stated about themselves or
  their life**; one short sentence each, in the third person ("Користувач
  працює вчителем."); skip one-off states, questions, things JARVIS said,
  and anything secret (passwords, keys, card or document numbers); reply
  with one JSON object `{"facts": ["…"]}` and nothing else; `{"facts": []}`
  when there is nothing.
- Parsing is lenient: reasoning blocks are stripped and the first JSON
  object in the reply is used (code fences and text around it are
  ignored). Items must be strings of 3–300 characters;
  more than 5 are cut to 5.
- Local filtering before saving: drop items whose normalized text equals an
  existing fact, a pending or a rejected suggestion. Existing facts are not
  sent to the model.
- Suggestions take the conversation's project. The cursor moves to the last
  message looked at when the model answered (valid JSON or not); it stays
  on transport errors, timeouts, privacy refusals and cancellation.

### 4. Privacy

`routing::check_data_privacy(provider, privacy, false, false)` before the
request, as for conversation summaries: the request contains only this
conversation, so it needs the same permission as chatting with that model.
A refusal emits a `Security` warning "Blocked fact suggestions: …".

### 5. IPC and events

| Command | Arguments | Result |
| --- | --- | --- |
| `list_fact_suggestions` | – | `FactSuggestion[]` (pending, newest first) |
| `accept_fact_suggestion` | `id`, `content`, `pinned`, `projectId?` | `Fact` |
| `reject_fact_suggestion` | `id` | – |

`FactSuggestion { id, content, projectId?, sourceConversationId,
sourceConversationTitle, createdAtMs }`. Event `jarvis://fact-suggestions`
carries the pending count after every change. An `Info` event in category
`Memory`: "JARVIS suggested N facts from a conversation (model)". Setting
`memory.suggestFacts: bool`, default `false`.

### 6. Interface

- Memory → Data: checkbox "Suggest facts from conversations" next to
  "Summarize long conversations", with a hint that each check is one model
  request (tens of seconds on a CPU) and that nothing is saved without
  review.
- Memory → Facts: a "Suggested (N)" section above the facts when N > 0;
  each item shows an editable text, the source conversation (link), a
  "Pinned" toggle, a project selector and **Accept** / **Reject**.
- Navigation: a number next to "Memory" while N > 0, from the event (the
  store reads the list once at start).

### 7. Errors

The chat never waits for or fails because of a suggestion request. Errors
are logged (`warn`) without message text. Database errors leave the cursor
unchanged. A suggestion whose conversation was deleted meanwhile fails to
accept with a clear message and disappears from the list.

## Testing

- `jarvis-storage`/`jarvis-memory`: migration on a v4 database with data;
  create, list, accept (fact created with the edited text and options,
  suggestion gone), reject (kept, filtered as a duplicate later); pending
  suggestions never appear in context assembly, search, the indexer queue or
  export; conversation deletion and `clear` remove them; normalization.
- `jarvis-core` with a scripted provider and a paused Tokio clock: no request
  before 120 s of quiet; new activity postpones it; only messages after the
  cursor are sent and user messages are the marked source; cursor rules
  (moves on an answer, stays on errors); temporary chats and disabled memory
  skipped; cloud provider without permission refused with a warning; at most
  5 items, length bounds, duplicates dropped; the count event.
- Frontend: store (list, accept, reject, count event), Facts panel section,
  Data panel checkbox, navigation counter.
- Live (test profile, Qwen2.5 3B in the embedded runtime): tell JARVIS a few
  things about yourself in Ukrainian, wait two minutes, check the
  suggestions, accept one and see it used in the next answer; record how many
  suggestions were correct, wrong or missing.

## Documentation

ARCHITECTURE (memory section, IPC table, the "Facts are saved explicitly"
limitation), ADR 0009 addendum, CHANGELOG, ROADMAP Phase 4.

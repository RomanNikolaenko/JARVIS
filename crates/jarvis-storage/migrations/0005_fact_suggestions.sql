-- Facts JARVIS suggests from conversations. They wait for the user's review
-- in their own table, so context assembly, search, the embedding indexer and
-- export (which read `facts`) never see them. Rejected suggestions are kept
-- so the same text is not suggested again; all of them go with their
-- conversation.
CREATE TABLE fact_suggestions (
    id                     TEXT PRIMARY KEY,
    content                TEXT NOT NULL,
    -- lowercase, collapsed whitespace, no trailing punctuation
    normalized             TEXT NOT NULL,
    project_id             TEXT REFERENCES projects (id) ON DELETE SET NULL,
    source_conversation_id TEXT NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
    status                 TEXT NOT NULL CHECK (status IN ('pending', 'rejected')),
    created_at             INTEGER NOT NULL
);
CREATE INDEX fact_suggestions_by_status ON fact_suggestions (status, created_at);

-- messages.id of the last message already looked at for facts
ALTER TABLE conversations ADD COLUMN facts_checked_until INTEGER NOT NULL DEFAULT 0;

-- Memory: projects, conversations, messages, conversation summaries, facts,
-- episodes and embeddings. Full-text indexes (FTS5, external content) are
-- kept in sync by triggers; deleting an item also deletes its embeddings.
--
-- Ids exposed outside the database are UUID strings (`id` for projects and
-- conversations, `uid` for rows that also have an integer rowid for FTS).
-- Timestamps are Unix milliseconds.

CREATE TABLE projects (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    description  TEXT NOT NULL DEFAULT '',
    instructions TEXT NOT NULL DEFAULT '',
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);

CREATE TABLE conversations (
    id          TEXT PRIMARY KEY,
    title       TEXT NOT NULL,
    project_id  TEXT REFERENCES projects (id) ON DELETE SET NULL,
    pinned_note TEXT NOT NULL DEFAULT '',
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);
CREATE INDEX conversations_by_update ON conversations (updated_at DESC);
CREATE INDEX conversations_by_project ON conversations (project_id);

CREATE TABLE embeddings (
    item_kind TEXT NOT NULL CHECK (item_kind IN ('message', 'fact')),
    item_id   INTEGER NOT NULL,
    model_key TEXT NOT NULL,
    vector    BLOB NOT NULL,
    PRIMARY KEY (item_kind, item_id, model_key)
) WITHOUT ROWID;

CREATE TABLE messages (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    uid             TEXT NOT NULL UNIQUE,
    conversation_id TEXT NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
    role            TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
    content         TEXT NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('complete', 'cancelled')),
    provider_id     TEXT,
    model           TEXT,
    tokens          INTEGER NOT NULL,
    created_at      INTEGER NOT NULL
);
CREATE INDEX messages_by_conversation ON messages (conversation_id, id);

CREATE VIRTUAL TABLE messages_fts USING fts5 (
    content,
    content = 'messages',
    content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 2'
);
CREATE TRIGGER messages_after_insert AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts (rowid, content) VALUES (new.id, new.content);
END;
CREATE TRIGGER messages_after_delete AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts (messages_fts, rowid, content) VALUES ('delete', old.id, old.content);
    DELETE FROM embeddings WHERE item_kind = 'message' AND item_id = old.id;
END;
CREATE TRIGGER messages_after_update AFTER UPDATE OF content ON messages BEGIN
    INSERT INTO messages_fts (messages_fts, rowid, content) VALUES ('delete', old.id, old.content);
    INSERT INTO messages_fts (rowid, content) VALUES (new.id, new.content);
    DELETE FROM embeddings WHERE item_kind = 'message' AND item_id = old.id;
END;

-- Running summary of the older part of a conversation (compaction).
CREATE TABLE summaries (
    conversation_id TEXT PRIMARY KEY REFERENCES conversations (id) ON DELETE CASCADE,
    content         TEXT NOT NULL,
    -- messages.id of the last message the summary covers
    covers_until    INTEGER NOT NULL,
    model           TEXT,
    tokens          INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);

CREATE TABLE facts (
    id                     INTEGER PRIMARY KEY AUTOINCREMENT,
    uid                    TEXT NOT NULL UNIQUE,
    content                TEXT NOT NULL,
    project_id             TEXT REFERENCES projects (id) ON DELETE CASCADE,
    pinned                 INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
    source                 TEXT NOT NULL CHECK (source IN ('user', 'conversation', 'import')),
    source_conversation_id TEXT REFERENCES conversations (id) ON DELETE SET NULL,
    created_at             INTEGER NOT NULL,
    updated_at             INTEGER NOT NULL
);
CREATE INDEX facts_by_project ON facts (project_id);

CREATE VIRTUAL TABLE facts_fts USING fts5 (
    content,
    content = 'facts',
    content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 2'
);
CREATE TRIGGER facts_after_insert AFTER INSERT ON facts BEGIN
    INSERT INTO facts_fts (rowid, content) VALUES (new.id, new.content);
END;
CREATE TRIGGER facts_after_delete AFTER DELETE ON facts BEGIN
    INSERT INTO facts_fts (facts_fts, rowid, content) VALUES ('delete', old.id, old.content);
    DELETE FROM embeddings WHERE item_kind = 'fact' AND item_id = old.id;
END;
CREATE TRIGGER facts_after_update AFTER UPDATE OF content ON facts BEGIN
    INSERT INTO facts_fts (facts_fts, rowid, content) VALUES ('delete', old.id, old.content);
    INSERT INTO facts_fts (rowid, content) VALUES (new.id, new.content);
    DELETE FROM embeddings WHERE item_kind = 'fact' AND item_id = old.id;
END;

-- Actions JARVIS performed (episodic memory).
CREATE TABLE episodes (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    uid         TEXT NOT NULL UNIQUE,
    occurred_at INTEGER NOT NULL,
    category    TEXT NOT NULL,
    level       TEXT NOT NULL,
    summary     TEXT NOT NULL
);
CREATE INDEX episodes_by_time ON episodes (occurred_at DESC);

CREATE VIRTUAL TABLE episodes_fts USING fts5 (
    summary,
    content = 'episodes',
    content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 2'
);
CREATE TRIGGER episodes_after_insert AFTER INSERT ON episodes BEGIN
    INSERT INTO episodes_fts (rowid, summary) VALUES (new.id, new.summary);
END;
CREATE TRIGGER episodes_after_delete AFTER DELETE ON episodes BEGIN
    INSERT INTO episodes_fts (episodes_fts, rowid, summary) VALUES ('delete', old.id, old.summary);
END;

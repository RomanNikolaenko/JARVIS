-- Mentor sessions: a conversation in which JARVIS teaches the user while
-- they build a project in a workspace folder. The session goes with its
-- conversation.
CREATE TABLE mentor_sessions (
    conversation_id TEXT PRIMARY KEY REFERENCES conversations (id) ON DELETE CASCADE,
    project_folder  TEXT NOT NULL,
    created_at      INTEGER NOT NULL
);

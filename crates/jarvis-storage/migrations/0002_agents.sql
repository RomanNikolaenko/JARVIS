-- Agents: tasks with their plan and step journal, and the tamper-evident
-- audit log of tool calls.

CREATE TABLE agent_tasks (
    id          TEXT PRIMARY KEY,
    goal        TEXT NOT NULL,
    status      TEXT NOT NULL CHECK (status IN (
                    'pending', 'running', 'waitingForApproval', 'paused',
                    'completed', 'failed', 'cancelled')),
    provider_id TEXT,
    model       TEXT,
    -- JSON: limits, plan and step journal
    limits      TEXT NOT NULL,
    plan        TEXT NOT NULL,
    steps       TEXT NOT NULL,
    result      TEXT,
    error       TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    finished_at INTEGER
);
CREATE INDEX agent_tasks_by_update ON agent_tasks (updated_at DESC);

-- Each entry hashes the previous one: rewriting or deleting an entry in the
-- middle breaks the chain, which `verify` detects.
CREATE TABLE audit_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    uid         TEXT NOT NULL UNIQUE,
    occurred_at INTEGER NOT NULL,
    task_id     TEXT,
    tool        TEXT NOT NULL,
    action      TEXT NOT NULL,
    risk        TEXT NOT NULL,
    event       TEXT NOT NULL,
    detail      TEXT NOT NULL,
    prev_hash   TEXT NOT NULL,
    hash        TEXT NOT NULL
);
CREATE INDEX audit_log_by_time ON audit_log (occurred_at DESC);

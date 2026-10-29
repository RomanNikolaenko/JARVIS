-- Preserve existing task journals while allowing an explicitly unverified answer.
CREATE TABLE agent_tasks_v4 (
    id TEXT PRIMARY KEY,
    goal TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN (
        'pending', 'running', 'waitingForApproval', 'paused',
        'completed', 'unverified', 'failed', 'cancelled')),
    provider_id TEXT,
    model TEXT,
    limits TEXT NOT NULL,
    plan TEXT NOT NULL,
    steps TEXT NOT NULL,
    result TEXT,
    error TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    finished_at INTEGER
);
INSERT INTO agent_tasks_v4 SELECT * FROM agent_tasks;
DROP TABLE agent_tasks;
ALTER TABLE agent_tasks_v4 RENAME TO agent_tasks;
CREATE INDEX agent_tasks_by_update ON agent_tasks (updated_at DESC);

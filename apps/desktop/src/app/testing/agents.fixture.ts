import type { AgentStatus, AgentTask, AuditEntry, PendingApproval, RiskLevel, TaskStatus, ToolInfo } from '@jarvis/shared-types';

export function agentTask(id: string, status: TaskStatus, updatedAtMs = 1): AgentTask {
  return {
    id,
    goal: `goal ${id}`,
    status,
    providerId: 'local-llama',
    model: 'qwen',
    limits: { maxSteps: 12, maxDurationSecs: 600 },
    plan: ['Look', 'Answer'],
    steps: [
      {
        index: 0,
        kind: 'action',
        thought: 'look first',
        tool: 'list_directory',
        input: '{"path":"."}',
        summary: 'List /work',
        decision: 'allowed (read-only)',
        outcome: 'succeeded',
        output: 'a.txt',
        startedAtMs: 1,
        durationMs: 12,
      },
    ],
    result: status === 'completed' ? 'Done: **1 file**' : null,
    error: status === 'failed' ? 'The step limit (12) was reached before the goal' : null,
    createdAtMs: 1,
    updatedAtMs,
    finishedAtMs: null,
  };
}

export function approval(id: string, risk: RiskLevel = 'medium', taskId = 't1'): PendingApproval {
  return {
    id,
    taskId,
    tool: risk === 'high' ? 'delete_path' : 'write_file',
    risk,
    summary: `${risk} action ${id}`,
    reason: `${risk} risk action`,
    details: '{\n  "path": "out.txt"\n}',
    requestedAtMs: Date.now(),
    expiresAtMs: Date.now() + 120_000,
  };
}

export const AGENT_STATUS: AgentStatus = {
  enabled: true,
  persistent: true,
  runningTasks: 0,
  workspaceFolders: [{ path: '/work', available: true }],
  launchedPrograms: [],
};

export function tool(name: string, risk: RiskLevel, enabled: boolean): ToolInfo {
  return {
    spec: {
      name,
      title: `Title ${name}`,
      description: `Does ${name}.`,
      parameters: {},
      capabilities: ['fsRead'],
      risk,
      idempotent: risk === 'low',
    },
    enabled,
  };
}

export function auditEntry(id: string, occurredAtMs: number): AuditEntry {
  return {
    id,
    occurredAtMs,
    taskId: 't1',
    tool: 'read_file',
    action: 'Read a.txt',
    risk: 'low',
    event: 'succeeded',
    detail: 'read 5 bytes',
  };
}

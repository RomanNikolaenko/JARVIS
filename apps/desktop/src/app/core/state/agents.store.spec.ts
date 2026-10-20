import { TestBed } from '@angular/core/testing';
import type { AgentStartRequest, AgentTask, PendingApproval } from '@jarvis/shared-types';

import { AGENT_STATUS, agentTask, approval, auditEntry } from '../../testing/agents.fixture';
import { SETTINGS } from '../../testing/settings.fixture';
import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';
import { AUDIT_PAGE, AgentsStore, summarize } from './agents.store';
import { SettingsStore } from './settings.store';

class FakeApi {
  onTask: ((task: AgentTask) => void) | null = null;
  onPending: ((approvals: PendingApproval[]) => void) | null = null;
  resolved: [string, boolean][] = [];
  started: AgentStartRequest[] = [];
  auditRequests: (number | null)[] = [];

  onAgentTask = vi.fn(async (handler: (task: AgentTask) => void) => {
    this.onTask = handler;
    return () => undefined;
  });
  onApprovals = vi.fn(async (handler: (approvals: PendingApproval[]) => void) => {
    this.onPending = handler;
    return () => undefined;
  });
  listPendingApprovals = vi.fn(async () => [approval('a1')]);
  getAgentStatus = vi.fn(async () => AGENT_STATUS);
  listTools = vi.fn(async () => []);
  listAgentTasks = vi.fn(async () => [summarize(agentTask('t1', 'completed', 5))]);
  getAgentTask = vi.fn(async (id: string) => agentTask(id, 'completed', 5));
  async startAgentTask(request: AgentStartRequest): Promise<AgentTask> {
    this.started.push(request);
    if (!request.goal) {
      throw new IpcError('validation', 'the goal is empty');
    }
    return agentTask('t2', 'pending', 10);
  }
  async resolveApproval(id: string, approved: boolean): Promise<boolean> {
    this.resolved.push([id, approved]);
    return true;
  }
  async getAuditLog(limit: number, beforeMs: number | null) {
    this.auditRequests.push(beforeMs);
    const start = beforeMs ?? 10_000;
    const count = beforeMs === null ? limit : 2;
    return Array.from({ length: count }, (_, i) => auditEntry(`e${start - i}`, start - i - 1));
  }
  verifyAuditLog = vi.fn(async () => ({ entries: 102, intact: true, brokenAt: null }));
  clearFinishedAgentTasks = vi.fn(async () => 1);
  addWorkspaceFolder = vi.fn(async () => ({
    ...SETTINGS,
    tools: { ...SETTINGS.tools, workspaceFolders: ['/new'] },
  }));
}

describe('AgentsStore', () => {
  let api: FakeApi;
  let store: AgentsStore;

  beforeEach(() => {
    api = new FakeApi();
    TestBed.configureTestingModule({ providers: [{ provide: JarvisApi, useValue: api }] });
    store = TestBed.inject(AgentsStore);
  });

  it('subscribes once and applies pushed tasks and approvals', async () => {
    await store.start();
    await store.start();
    expect(api.onAgentTask).toHaveBeenCalledTimes(1);
    expect(store.approvals().map((a) => a.id)).toEqual(['a1']);

    await store.loadTasks();
    expect(store.selectedId()).toBe('t1');
    expect(store.selected()?.status).toBe('completed');

    api.onTask!(agentTask('t3', 'running', 20));
    api.onTask!({ ...agentTask('t1', 'completed', 5), goal: 'updated goal' });
    expect(store.tasks().map((t) => t.id)).toEqual(['t3', 't1']);
    expect(store.selected()?.goal).toBe('updated goal');
    expect(store.running()).toBe(1);

    api.onPending!([approval('a2'), approval('a3')]);
    expect(store.approvals().map((a) => a.id)).toEqual(['a2', 'a3']);
  });

  it('starts tasks, selects them and reports validation errors', async () => {
    const task = await store.startTask({
      goal: 'List files',
      providerId: null,
      model: null,
      maxSteps: null,
      maxDurationSecs: null,
    });
    expect(task?.id).toBe('t2');
    expect(store.selectedId()).toBe('t2');
    expect(store.tasks()[0].status).toBe('pending');

    const failed = await store.startTask({
      goal: '',
      providerId: null,
      model: null,
      maxSteps: null,
      maxDurationSecs: null,
    });
    expect(failed).toBeNull();
    expect(store.error()?.code).toBe('validation');
  });

  it('sends decisions and drops them from the queue', async () => {
    await store.start();
    await store.resolve('a1', false);
    expect(api.resolved).toEqual([['a1', false]]);
    expect(store.approvals()).toEqual([]);
  });

  it('pages through the audit log and verifies it', async () => {
    await store.loadAudit();
    expect(store.audit()).toHaveLength(AUDIT_PAGE);
    expect(store.auditComplete()).toBe(false);
    await store.loadAudit(true);
    expect(store.audit()).toHaveLength(AUDIT_PAGE + 2);
    expect(store.auditComplete()).toBe(true);
    expect(api.auditRequests[1]).toBe(store.audit()[AUDIT_PAGE - 1].occurredAtMs);

    await store.verifyAudit();
    expect(store.verification()?.intact).toBe(true);
  });

  it('applies settings returned by the folder dialog', async () => {
    const settings = TestBed.inject(SettingsStore);
    settings.settings.set(SETTINGS);
    await store.addWorkspaceFolder();
    expect(settings.settings()?.tools.workspaceFolders).toEqual(['/new']);
    expect(api.getAgentStatus).toHaveBeenCalled();
  });

  it('forgets the selection when finished tasks are cleared', async () => {
    await store.loadTasks();
    expect(store.selectedId()).toBe('t1');
    api.listAgentTasks.mockResolvedValueOnce([]);
    await store.clearFinished();
    expect(store.selected()).toBeNull();
    expect(store.tasks()).toEqual([]);
  });
});

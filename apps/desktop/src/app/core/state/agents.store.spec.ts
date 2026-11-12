import { TestBed } from '@angular/core/testing';
import type {
  AgentStartRequest,
  AgentTask,
  AuditEntry,
  AuditVerification,
  PendingApproval,
  TaskSummary,
} from '@jarvis/shared-types';

import { AGENT_STATUS, agentTask, approval, auditEntry } from '../../testing/agents.fixture';
import { SETTINGS } from '../../testing/settings.fixture';
import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';
import {
  AGENT_COMMAND_TIMEOUT_MS,
  AGENT_READ_TIMEOUT_MS,
  APPROVAL_DECISION_TIMEOUT_MS,
  AUDIT_PAGE,
  AUDIT_TIMEOUT_MS,
  AgentsStore,
  summarize,
} from './agents.store';
import { SettingsStore } from './settings.store';

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

const START_REQUEST: AgentStartRequest = {
  goal: 'List files',
  providerId: null,
  model: null,
  maxSteps: null,
  maxDurationSecs: null,
};

class FakeApi {
  onTask: ((task: AgentTask) => void) | null = null;
  onPending: ((approvals: PendingApproval[]) => void) | null = null;
  resolved: [string, boolean][] = [];
  started: AgentStartRequest[] = [];
  auditRequests: (string | null)[] = [];

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
      throw new IpcError('validation', 'the goal is empty', 'core');
    }
    return agentTask('t2', 'pending', 10);
  }
  async resolveApproval(id: string, approved: boolean): Promise<boolean> {
    this.resolved.push([id, approved]);
    return true;
  }
  pauseAgentTask = vi.fn(async (_id: string) => true);
  resumeAgentTask = vi.fn(async (_id: string) => undefined);
  cancelAgentTask = vi.fn(async (_id: string) => true);
  async getAuditLog(limit: number, beforeId: string | null) {
    this.auditRequests.push(beforeId);
    const start = beforeId === null ? 10_000 : Number(beforeId.slice(1)) - 1;
    const count = beforeId === null ? limit : 2;
    return Array.from({ length: count }, (_, i) => auditEntry(`e${start - i}`, start - i - 1));
  }
  verifyAuditLog = vi.fn(async (): Promise<AuditVerification> => ({ entries: 102, intact: true, brokenAt: null }));
  clearFinishedAgentTasks = vi.fn(async (): Promise<string[]> => []);
  deleteReviewedAgentTask = vi.fn(async (_id: string) => 1);
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

  it.each([
    new IpcError('permissionDenied', 'Cloud agents are disabled', 'core'),
    new IpcError('privacyBlocked', 'Cloud sharing is disabled', 'core'),
    new IpcError('unavailable', 'Desktop bridge is not reachable', 'notDispatched'),
  ])('preserves an exact confirmed start refusal: %s', async (failure) => {
    const start = vi.spyOn(api, 'startAgentTask').mockRejectedValueOnce(failure);
    expect(await store.startTask(START_REQUEST)).toBeNull();
    expect(store.error()).toBe(failure);
    expect(store.tasks()).toEqual([]);
    expect(store.selectedId()).toBeNull();
    expect(start).toHaveBeenCalledExactlyOnceWith(START_REQUEST);
  });

  it.each([
    new IpcError('internal', 'Transport disconnected', 'transport'),
    new IpcError('io', 'Storage acknowledgement failed', 'core'),
    new IpcError('internal', 'Core reply failed', 'core'),
  ])('reports an ambiguous start outcome without retrying: %s', async (failure) => {
    const start = vi.spyOn(api, 'startAgentTask').mockRejectedValueOnce(failure);
    expect(await store.startTask(START_REQUEST)).toBeNull();
    expect(store.error()).toMatchObject({ code: failure.code, origin: failure.origin });
    expect(store.error()?.message).toContain(failure.message);
    expect(store.error()?.message).toContain('Task creation could not be confirmed');
    expect(store.error()?.message).toContain('may already be running or may start later');
    expect(store.error()?.message).toContain('Use Refresh tasks in Agents to check before starting another task');
    expect(start).toHaveBeenCalledExactlyOnceWith(START_REQUEST);
    expect(store.tasks()).toEqual([]);
    expect(store.selectedId()).toBeNull();
    expect(store.busy()).toBe(false);
  });

  it('releases a hung start and leaves late task selection to the user', async () => {
    vi.useFakeTimers();
    try {
      await store.start();
      await store.select('open-journal');
      const response = deferred<AgentTask>();
      const start = vi.spyOn(api, 'startAgentTask').mockReturnValueOnce(response.promise);
      const shouldSelect = vi.fn(() => true);
      const pending = store.startTask(START_REQUEST, shouldSelect);
      expect(store.busy()).toBe(true);

      await vi.advanceTimersByTimeAsync(AGENT_COMMAND_TIMEOUT_MS);
      expect(await pending).toBeNull();
      expect(store.busy()).toBe(false);
      expect(store.error()?.code).toBe('timeout');
      expect(store.error()?.message).toContain('may already be running or may start later');
      expect(start).toHaveBeenCalledExactlyOnceWith(START_REQUEST);

      const created = agentTask('late', 'running');
      response.resolve(created);
      await Promise.resolve();
      expect(shouldSelect).not.toHaveBeenCalled();
      expect(store.selectedId()).toBe('open-journal');
      expect(store.tasks()).toEqual([]);
      api.onTask!(created);
      expect(store.tasks()).toEqual([summarize(created)]);
      expect(store.selectedId()).toBe('open-journal');
    } finally {
      vi.useRealTimers();
    }
  });

  it('coalesces duplicate pause requests while another task and Cancel remain available', async () => {
    const pause = deferred<boolean>();
    api.pauseAgentTask.mockReturnValueOnce(pause.promise);
    const pending = store.pause('first');
    expect(store.isControlling('pause', 'first')).toBe(true);
    expect(store.pause('first')).toBe(pending);
    expect(store.isControlling('pause', 'second')).toBe(false);
    expect(store.isControlling('cancel', 'first')).toBe(false);

    await Promise.all([store.pause('second'), store.cancel('first')]);
    expect(api.pauseAgentTask.mock.calls).toEqual([['first'], ['second']]);
    expect(api.cancelAgentTask).toHaveBeenCalledExactlyOnceWith('first');
    expect(store.isControlling('pause', 'first')).toBe(true);
    expect(store.busy()).toBe(true);

    pause.resolve(true);
    await pending;
    expect(store.isControlling('pause', 'first')).toBe(false);
    expect(store.busy()).toBe(false);
  });

  it('bounds a hung resume while preserving cancellation and the live journal', async () => {
    vi.useFakeTimers();
    try {
      await store.start();
      await store.select('t1');
      api.onTask!(agentTask('t1', 'paused', 10));
      const resume = deferred<undefined>();
      api.resumeAgentTask.mockReturnValueOnce(resume.promise);
      const pending = store.resume('t1');
      expect(store.resume('t1')).toBe(pending);
      expect(store.isControlling('resume', 't1')).toBe(true);
      await store.cancel('t1');
      expect(api.cancelAgentTask).toHaveBeenCalledExactlyOnceWith('t1');
      api.onTask!(agentTask('t1', 'cancelled', 20));

      await vi.advanceTimersByTimeAsync(AGENT_COMMAND_TIMEOUT_MS);
      await pending;
      const warning = store.error();
      expect(warning?.code).toBe('timeout');
      expect(warning?.message).toContain('The resume request could not be confirmed');
      expect(store.isControlling('resume', 't1')).toBe(false);
      expect(store.busy()).toBe(false);

      resume.resolve(undefined);
      await Promise.resolve();
      expect(store.error()).toBe(warning);
      expect(store.selected()?.status).toBe('cancelled');
      expect(api.resumeAgentTask).toHaveBeenCalledExactlyOnceWith('t1');
    } finally {
      vi.useRealTimers();
    }
  });

  it.each(['pause', 'cancel'] as const)('reports a declined %s without changing the journal', async (control) => {
    await store.select('t1');
    const journal = store.selected();
    api.pauseAgentTask.mockResolvedValueOnce(false);
    api.cancelAgentTask.mockResolvedValueOnce(false);
    await store[control]('t1');
    expect(store.error()?.message).toContain(`This task is no longer accepting ${control} requests`);
    expect(store.selected()).toBe(journal);
    expect(store.isControlling(control, 't1')).toBe(false);
  });

  it('does not infer a successful start from another task with the same goal after an uncertain failure', async () => {
    await store.start();
    const response = deferred<AgentTask>();
    const start = vi.spyOn(api, 'startAgentTask').mockReturnValueOnce(response.promise);
    const pending = store.startTask(START_REQUEST);
    api.onTask!({ ...agentTask('observed', 'running', 10), goal: START_REQUEST.goal });
    response.reject(new Error('Connection closed'));
    expect(await pending).toBeNull();
    expect(store.tasks().map((task) => task.id)).toEqual(['observed']);
    expect(store.selectedId()).toBeNull();
    expect(store.error()).toMatchObject({ code: 'internal', origin: 'transport' });
    expect(store.error()?.message).toContain('could not be confirmed');
    expect(start).toHaveBeenCalledOnce();
  });

  it.each(['other', 'none', 'same', 'awayBack'] as const)(
    'retains a newly created task without replacing a newer journal choice: %s',
    async (choice) => {
      await store.select('original');
      const response = deferred<AgentTask>();
      const start = vi.spyOn(api, 'startAgentTask').mockReturnValueOnce(response.promise);
      const pending = store.startTask(START_REQUEST);
      if (choice === 'other') await store.select('other');
      else if (choice === 'none') await store.select(null);
      else if (choice === 'same') await store.select('original');
      else {
        await store.select('other');
        await store.select('original');
      }
      const selected = store.selected();
      const selectedId = store.selectedId();
      response.resolve(agentTask('created', 'pending', 10));
      expect((await pending)?.id).toBe('created');
      expect(store.selectedId()).toBe(selectedId);
      expect(store.selected()).toBe(selected);
      expect(store.tasks().map((task) => task.id)).toContain('created');
      expect(start).toHaveBeenCalledExactlyOnceWith(START_REQUEST);
      expect(store.busy()).toBe(false);
    }
  );

  it('uses the originating editor predicate at acknowledgement without dropping its confirmed task', async () => {
    await store.select('original');
    const selected = store.selected();
    const response = deferred<AgentTask>();
    vi.spyOn(api, 'startAgentTask').mockReturnValueOnce(response.promise);
    let current = true;
    const shouldSelect = vi.fn(() => current);
    const pending = store.startTask(START_REQUEST, shouldSelect);
    current = false;
    response.resolve(agentTask('created', 'pending', 10));
    expect((await pending)?.id).toBe('created');
    expect(shouldSelect).toHaveBeenCalledOnce();
    expect(store.selected()).toBe(selected);
    expect(store.selectedId()).toBe('original');
    expect(store.tasks().map((task) => task.id)).toContain('created');
  });

  it('keeps a newer explicitly selected journal for the created task instead of downgrading it from the start acknowledgement', async () => {
    const response = deferred<AgentTask>();
    vi.spyOn(api, 'startAgentTask').mockReturnValueOnce(response.promise);
    const completed = agentTask('created', 'completed', 20);
    api.getAgentTask.mockResolvedValueOnce(completed);
    const pending = store.startTask(START_REQUEST);
    await store.select('created');
    response.resolve(agentTask('created', 'pending', 10));
    expect(await pending).toBe(completed);
    expect(store.selected()).toBe(completed);
    expect(store.selectedId()).toBe('created');
    expect(store.tasks().find((task) => task.id === 'created')).toMatchObject({ status: 'completed', updatedAtMs: 20 });
  });

  it('does not treat a creation acknowledgement as a newer event for a pending journal read', async () => {
    const response = deferred<AgentTask>();
    const journal = deferred<AgentTask>();
    vi.spyOn(api, 'startAgentTask').mockReturnValueOnce(response.promise);
    api.getAgentTask.mockReturnValueOnce(journal.promise);
    const pending = store.startTask(START_REQUEST);
    const selecting = store.select('created');
    response.resolve(agentTask('created', 'pending', 10));
    await pending;
    expect(store.selectedId()).toBe('created');
    expect(store.selected()).toBeNull();
    const completed = agentTask('created', 'completed', 20);
    journal.resolve(completed);
    await selecting;
    expect(store.selected()).toBe(completed);
  });

  it('retains acknowledged creation when an older task-list snapshot arrives afterwards', async () => {
    const list = deferred<TaskSummary[]>();
    const response = deferred<AgentTask>();
    api.listAgentTasks.mockReturnValueOnce(list.promise);
    vi.spyOn(api, 'startAgentTask').mockReturnValueOnce(response.promise);
    const loading = store.loadTasks();
    const pending = store.startTask(START_REQUEST);
    response.resolve(agentTask('created', 'pending', 10));
    await pending;
    list.resolve([]);
    await loading;
    expect(store.tasks().map((task) => task.id)).toEqual(['created']);
    expect(store.selectedId()).toBe('created');
  });

  it('selects the confirmed task when the live originating editor predicate still permits it', async () => {
    const shouldSelect = vi.fn(() => true);
    const task = await store.startTask(START_REQUEST, shouldSelect);
    expect(shouldSelect).toHaveBeenCalledOnce();
    expect(store.selectedId()).toBe(task?.id);
    expect(store.selected()).toBe(task);
  });

  it.each(['olderFirst', 'newerFirst'] as const)(
    'selects only the latest start intent when acknowledgements arrive %s',
    async (order) => {
      const olderResponse = deferred<AgentTask>();
      const newerResponse = deferred<AgentTask>();
      const start = vi
        .spyOn(api, 'startAgentTask')
        .mockReturnValueOnce(olderResponse.promise)
        .mockReturnValueOnce(newerResponse.promise);
      const older = store.startTask({ ...START_REQUEST, goal: 'Older goal' });
      const newer = store.startTask({ ...START_REQUEST, goal: 'Newer goal' });
      if (order === 'olderFirst') {
        olderResponse.resolve(agentTask('older', 'pending', 10));
        await older;
        expect(store.selectedId()).toBeNull();
        newerResponse.resolve(agentTask('newer', 'pending', 11));
        await newer;
      } else {
        newerResponse.resolve(agentTask('newer', 'pending', 11));
        await newer;
        olderResponse.resolve(agentTask('older', 'pending', 10));
        await older;
      }
      expect(store.selectedId()).toBe('newer');
      expect(store.selected()?.id).toBe('newer');
      expect(store.tasks().map((task) => task.id)).toEqual(['newer', 'older']);
      expect(start).toHaveBeenCalledTimes(2);
    }
  );

  it('preserves an early completion event while a later journal choice prevents automatic selection', async () => {
    await store.start();
    const response = deferred<AgentTask>();
    vi.spyOn(api, 'startAgentTask').mockReturnValueOnce(response.promise);
    const pending = store.startTask(START_REQUEST);
    const completed = agentTask('created', 'completed', 10);
    api.onTask!(completed);
    await store.select('other');
    response.resolve(agentTask('created', 'pending', 10));
    expect(await pending).toBe(completed);
    expect(store.selectedId()).toBe('other');
    expect(store.selected()?.id).toBe('other');
    expect(store.tasks().find((task) => task.id === 'created')?.status).toBe('completed');
  });

  it('ignores a late start acknowledgement after destruction and refuses further starts', async () => {
    await store.select('original');
    const tasks = store.tasks();
    const selected = store.selected();
    const response = deferred<AgentTask>();
    const start = vi.spyOn(api, 'startAgentTask').mockReturnValueOnce(response.promise);
    const pending = store.startTask(START_REQUEST);
    TestBed.resetTestingModule();
    response.resolve(agentTask('late', 'pending', 10));
    expect(await pending).toBeNull();
    expect(store.tasks()).toBe(tasks);
    expect(store.selected()).toBe(selected);
    expect(store.selectedId()).toBe('original');
    expect(await store.startTask(START_REQUEST)).toBeNull();
    expect(start).toHaveBeenCalledOnce();
  });

  it('shares an in-flight subscription attempt between callers', async () => {
    const response = deferred<() => undefined>();
    api.onAgentTask.mockReturnValueOnce(response.promise);
    const first = store.start();
    const second = store.start();
    expect(second).toBe(first);
    expect(store.connecting()).toBe(true);
    expect(api.onAgentTask).toHaveBeenCalledTimes(1);
    response.resolve(() => undefined);
    await Promise.all([first, second]);
    expect(api.onApprovals).toHaveBeenCalledTimes(1);
    expect(store.connecting()).toBe(false);
  });

  it('removes partial subscriptions before retry and ignores their queued events', async () => {
    const unlisten = vi.fn(() => undefined);
    api.onAgentTask.mockImplementationOnce(async (handler) => {
      api.onTask = handler;
      return unlisten;
    });
    api.onApprovals.mockRejectedValueOnce(new IpcError('unavailable', 'Subscription failed'));
    await store.start();
    const staleHandler = api.onTask!;
    expect(unlisten).toHaveBeenCalledTimes(1);
    expect(store.subscriptionError()?.message).toBe('Subscription failed');
    await store.refreshStatus();
    expect(store.subscriptionError()).not.toBeNull();
    await store.start();
    staleHandler(agentTask('stale', 'running'));
    api.onTask!(agentTask('current', 'running'));
    expect(store.tasks().some((t) => t.id === 'current')).toBe(true);
    expect(store.tasks().some((t) => t.id === 'stale')).toBe(false);
    expect(store.subscriptionError()).toBeNull();
    expect(api.onAgentTask).toHaveBeenCalledTimes(2);
  });

  it.each(['task', 'approval'] as const)('times out %s registration and releases late listeners after retry', async (stage) => {
    vi.useFakeTimers();
    try {
      const registration = deferred<() => undefined>();
      const lateCleanup = vi.fn(() => undefined);
      const partialCleanup = vi.fn(() => undefined);
      let staleEvent!: () => void;
      if (stage === 'task') {
        api.onAgentTask.mockImplementationOnce((handler) => {
          staleEvent = () => handler(agentTask('stale', 'running'));
          return registration.promise;
        });
      } else {
        api.onAgentTask.mockResolvedValueOnce(partialCleanup);
        api.onApprovals.mockImplementationOnce((handler) => {
          staleEvent = () => handler([approval('stale')]);
          return registration.promise;
        });
      }
      const starting = store.start();
      expect(store.start()).toBe(starting);
      await vi.advanceTimersByTimeAsync(AGENT_READ_TIMEOUT_MS);
      await starting;
      expect(store.connecting()).toBe(false);
      expect(store.subscriptionError()?.code).toBe('timeout');
      expect(partialCleanup).toHaveBeenCalledTimes(stage === 'approval' ? 1 : 0);
      await store.start();
      expect(store.subscriptionError()).toBeNull();
      staleEvent();
      registration.resolve(lateCleanup);
      await vi.advanceTimersByTimeAsync(0);
      expect(lateCleanup).toHaveBeenCalledTimes(1);
      expect(store.tasks().some((task) => task.id === 'stale')).toBe(false);
      expect(store.approvals().map((entry) => entry.id)).toEqual(['a1']);
      expect(store.subscriptionError()).toBeNull();
      TestBed.resetTestingModule();
      expect(lateCleanup).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it.each(['task', 'approval'] as const)(
    'settles pending %s registration on destruction before native completion',
    async (stage) => {
      const registration = deferred<() => undefined>();
      const cleanup = vi.fn(() => undefined);
      if (stage === 'task') api.onAgentTask.mockReturnValueOnce(registration.promise);
      else api.onApprovals.mockReturnValueOnce(registration.promise);
      const starting = store.start();
      if (stage === 'approval') await vi.waitFor(() => expect(api.onApprovals).toHaveBeenCalled());
      TestBed.resetTestingModule();
      await starting;
      expect(store.connecting()).toBe(false);
      expect(store.subscriptionError()).toBeNull();
      registration.resolve(cleanup);
      await registration.promise;
      expect(cleanup).toHaveBeenCalledTimes(1);
      expect(api.listPendingApprovals).not.toHaveBeenCalled();
    }
  );

  it('ignores late registration rejection after a successful retry', async () => {
    vi.useFakeTimers();
    try {
      const registration = deferred<() => undefined>();
      api.onAgentTask.mockReturnValueOnce(registration.promise);
      const first = store.start();
      await vi.advanceTimersByTimeAsync(AGENT_READ_TIMEOUT_MS);
      await first;
      await store.start();
      registration.reject(new Error('obsolete registration failed'));
      await vi.advanceTimersByTimeAsync(0);
      expect(store.subscriptionError()).toBeNull();
      expect(store.connecting()).toBe(false);
      await store.start();
      expect(api.onAgentTask).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it('withdraws early approvals and removes both listeners when the initial approval snapshot fails', async () => {
    const taskCleanup = vi.fn(() => undefined);
    const approvalCleanup = vi.fn(() => undefined);
    const snapshot = deferred<PendingApproval[]>();
    const lateReply = deferred<boolean>();
    api.onAgentTask.mockResolvedValueOnce(taskCleanup);
    api.onApprovals.mockImplementationOnce(async (handler) => {
      handler([approval('early')]);
      return approvalCleanup;
    });
    api.listPendingApprovals.mockReturnValueOnce(snapshot.promise);
    vi.spyOn(api, 'resolveApproval').mockReturnValueOnce(lateReply.promise);
    const starting = store.start();
    await vi.waitFor(() => expect(api.listPendingApprovals).toHaveBeenCalled());
    const deciding = store.resolve('early', true);
    snapshot.reject(new IpcError('unavailable', 'Snapshot failed'));
    await starting;
    await expect(deciding).resolves.toMatchObject({ message: expect.stringContaining('The decision could not be confirmed.') });
    expect(store.approvals()).toEqual([]);
    expect(store.decidingApprovalIds()).toEqual([]);
    expect(taskCleanup).toHaveBeenCalledTimes(1);
    expect(approvalCleanup).toHaveBeenCalledTimes(1);
    await store.start();
    lateReply.resolve(true);
    expect(store.subscriptionError()).toBeNull();
    expect(store.approvals().map((entry) => entry.id)).toEqual(['a1']);
  });

  async function failConnection() {
    api.onApprovals.mockRejectedValueOnce(new IpcError('unavailable', 'Disconnected'));
    await store.start();
  }

  it('withdraws stale approvals on connection loss and recovers without restarting a task', async () => {
    await store.start();
    await store.select('t1');
    const staleTask = api.onTask!;
    const staleApprovals = api.onPending!;
    store.connectionLost(new IpcError('unavailable', 'Offline'));
    expect(store.approvals()).toEqual([]);
    staleTask(agentTask('stale', 'running'));
    staleApprovals([approval('stale')]);
    expect(store.approvals()).toEqual([]);
    api.getAgentTask.mockResolvedValueOnce(agentTask('t1', 'completed', 70));
    await store.start();
    expect(store.selected()?.updatedAtMs).toBe(70);
    expect(store.tasks().some((t) => t.id === 'stale')).toBe(false);
    expect(api.started).toEqual([]);
    expect(store.subscriptionError()).toBeNull();
  });

  it('reconnects while an obsolete registration is unresolved', async () => {
    const old = deferred<() => undefined>();
    const current = deferred<() => undefined>();
    const cleanup = vi.fn(() => undefined);
    api.onAgentTask.mockReturnValueOnce(old.promise).mockReturnValueOnce(current.promise);
    const first = store.start();
    store.connectionLost(new IpcError('unavailable', 'Offline'));
    const second = store.start();
    expect(api.onAgentTask).toHaveBeenCalledTimes(2);
    old.resolve(cleanup);
    await first;
    expect(cleanup).toHaveBeenCalledTimes(1);
    expect(store.connecting()).toBe(true);
    expect(store.start()).toBe(second);
    current.resolve(() => undefined);
    await second;
    expect(store.connecting()).toBe(false);
    expect(store.subscriptionError()).toBeNull();
  });

  it('recovers the list and open journal after reconnecting', async () => {
    await store.select('t1');
    await failConnection();
    api.listAgentTasks.mockResolvedValueOnce([
      summarize(agentTask('t1', 'completed', 20)),
      summarize(agentTask('missed', 'running', 30)),
    ]);
    api.getAgentTask.mockResolvedValueOnce(agentTask('t1', 'completed', 20));
    await store.start();
    expect(store.tasks().map((t) => t.id)).toEqual(['missed', 't1']);
    expect(store.selected()?.updatedAtMs).toBe(20);
    expect(store.subscriptionError()).toBeNull();
  });

  it('keeps live updates during recovery and waits for the whole recovery', async () => {
    await store.select('t1');
    await failConnection();
    const response = deferred<TaskSummary[]>();
    api.listAgentTasks.mockReturnValueOnce(response.promise);
    const recovering = store.start();
    await vi.waitFor(() => expect(api.listAgentTasks).toHaveBeenCalled());
    api.onTask!(agentTask('t1', 'completed', 40));
    api.onTask!(agentTask('new', 'running', 50));
    expect(store.connecting()).toBe(true);
    response.resolve([summarize(agentTask('t1', 'running', 40))]);
    await recovering;
    expect(store.selected()?.updatedAtMs).toBe(40);
    expect(store.tasks().find((t) => t.id === 't1')?.status).toBe('completed');
    expect(store.tasks().some((t) => t.id === 'new')).toBe(true);
  });

  it('retains acknowledged creation when a recovery list snapshot predates it', async () => {
    await store.select('t1');
    await failConnection();
    const list = deferred<TaskSummary[]>();
    api.listAgentTasks.mockReturnValueOnce(list.promise);
    const recovering = store.start();
    await vi.waitFor(() => expect(api.listAgentTasks).toHaveBeenCalledOnce());
    const created = await store.startTask(START_REQUEST, () => false);
    list.resolve([summarize(agentTask('t1', 'completed', 5))]);
    await recovering;
    expect(store.tasks().map((task) => task.id)).toContain(created!.id);
    expect(store.selectedId()).toBe('t1');
  });

  it('does not let creation acknowledgement replace a newer recovery journal snapshot', async () => {
    await store.select('created');
    const response = deferred<AgentTask>();
    vi.spyOn(api, 'startAgentTask').mockReturnValueOnce(response.promise);
    const pending = store.startTask(START_REQUEST);
    await failConnection();
    const journal = deferred<AgentTask>();
    api.getAgentTask.mockReturnValueOnce(journal.promise);
    const recovering = store.start();
    await vi.waitFor(() => expect(api.getAgentTask).toHaveBeenCalledTimes(2));
    response.resolve(agentTask('created', 'pending', 10));
    await pending;
    const completed = agentTask('created', 'completed', 20);
    journal.resolve(completed);
    await recovering;
    expect(store.selectedId()).toBe('created');
    expect(store.selected()).toBe(completed);
  });

  it('does not change a selection made while recovery is pending', async () => {
    await store.select('t1');
    await failConnection();
    const response = deferred<AgentTask>();
    api.getAgentTask.mockReturnValueOnce(response.promise);
    const recovering = store.start();
    await vi.waitFor(() => expect(api.listAgentTasks).toHaveBeenCalled());
    await store.select('t2');
    response.resolve(agentTask('t1', 'completed', 20));
    await recovering;
    expect(store.selectedId()).toBe('t2');
    expect(store.selected()?.id).toBe('t2');
  });

  it('clears a journal deleted while disconnected despite queued updates for it', async () => {
    await store.select('t1');
    await failConnection();
    const response = deferred<AgentTask>();
    api.getAgentTask.mockReturnValueOnce(response.promise);
    api.listAgentTasks.mockResolvedValueOnce([]);
    const recovering = store.start();
    await vi.waitFor(() => expect(api.listAgentTasks).toHaveBeenCalled());
    const removed = agentTask('t1', 'completed', 20);
    api.onTask!(removed);
    response.reject(new IpcError('notFound', 'Deleted', 'core'));
    await recovering;
    api.onTask!(removed);
    expect(store.selectedId()).toBeNull();
    expect(store.selected()).toBeNull();
    expect(store.tasks()).toEqual([]);
    expect(store.subscriptionError()).toBeNull();
  });

  it('keeps recovery failures visible and retries the snapshot', async () => {
    await failConnection();
    api.listAgentTasks.mockRejectedValueOnce(new IpcError('unavailable', 'Recovery failed'));
    await store.start();
    expect(store.subscriptionError()?.message).toBe('Recovery failed');
    await store.start();
    expect(store.subscriptionError()).toBeNull();
    expect(store.tasks().map((t) => t.id)).toEqual(['t1']);
    expect(api.listAgentTasks).toHaveBeenCalledTimes(2);
  });

  it('does not publish recovery responses after destruction', async () => {
    await failConnection();
    const response = deferred<TaskSummary[]>();
    api.listAgentTasks.mockReturnValueOnce(response.promise);
    const recovering = store.start();
    await vi.waitFor(() => expect(api.listAgentTasks).toHaveBeenCalled());
    TestBed.resetTestingModule();
    response.resolve([summarize(agentTask('late', 'running'))]);
    await recovering;
    expect(store.tasks()).toEqual([]);
    expect(store.selected()).toBeNull();
  });

  it('cleans up listeners that finish registering after destruction', async () => {
    const response = deferred<() => undefined>();
    const unlisten = vi.fn(() => undefined);
    api.onAgentTask.mockReturnValueOnce(response.promise);
    const starting = store.start();
    TestBed.resetTestingModule();
    response.resolve(unlisten);
    await starting;
    expect(unlisten).toHaveBeenCalledTimes(1);
    expect(api.onApprovals).not.toHaveBeenCalled();
    await store.start();
    expect(api.onAgentTask).toHaveBeenCalledTimes(1);
  });

  it('ignores a pending snapshot after destruction and releases both listeners', async () => {
    const response = deferred<PendingApproval[]>();
    const taskCleanup = vi.fn(() => undefined);
    const approvalCleanup = vi.fn(() => undefined);
    api.onAgentTask.mockResolvedValueOnce(taskCleanup);
    api.onApprovals.mockResolvedValueOnce(approvalCleanup);
    api.listPendingApprovals.mockReturnValueOnce(response.promise);
    const starting = store.start();
    await vi.waitFor(() => expect(api.listPendingApprovals).toHaveBeenCalled());
    TestBed.resetTestingModule();
    response.resolve([approval('late')]);
    await starting;
    expect(taskCleanup).toHaveBeenCalledTimes(1);
    expect(approvalCleanup).toHaveBeenCalledTimes(1);
    expect(store.approvals()).toEqual([]);
  });

  it('sends decisions and drops them from the queue', async () => {
    await store.start();
    await store.resolve('a1', false);
    expect(api.resolved).toEqual([['a1', false]]);
    expect(store.approvals()).toEqual([]);
  });

  it('coalesces decisions for the same request while allowing a different request to proceed', async () => {
    await store.start();
    api.onPending!([approval('a1'), approval('a2')]);
    const first = deferred<boolean>();
    const resolveApproval = vi.spyOn(api, 'resolveApproval').mockReturnValueOnce(first.promise).mockResolvedValueOnce(false);

    const allowing = store.resolve('a1', true);
    expect(store.resolve('a1', false)).toBe(allowing);
    const denying = store.resolve('a2', false);
    expect(store.decidingApprovalIds()).toEqual(['a1', 'a2']);
    expect(resolveApproval.mock.calls).toEqual([
      ['a1', true],
      ['a2', false],
    ]);

    expect(await denying).toBeNull();
    expect(store.approvals().map((entry) => entry.id)).toEqual(['a1']);
    expect(store.decidingApprovalIds()).toEqual(['a1']);
    first.resolve(true);
    expect(await allowing).toBeNull();
    expect(store.approvals()).toEqual([]);
    expect(store.decidingApprovalIds()).toEqual([]);
  });

  it('refuses missing or expired permission requests before dispatch', async () => {
    await store.start();
    api.onPending!([{ ...approval('expired'), expiresAtMs: Date.now() }]);

    await expect(store.resolve('missing', true)).resolves.toMatchObject({ code: 'notFound' });
    await expect(store.resolve('expired', true)).resolves.toMatchObject({
      code: 'unavailable',
      message: expect.stringContaining('expired'),
    });
    expect(store.decidingApprovalIds()).toEqual([]);
    expect(api.resolved).toEqual([]);
  });

  it('bounds a hung decision and ignores its late acknowledgement', async () => {
    vi.useFakeTimers();
    try {
      await store.start();
      const late = deferred<boolean>();
      const resolveApproval = vi.spyOn(api, 'resolveApproval').mockReturnValueOnce(late.promise);
      const deciding = store.resolve('a1', true);
      expect(store.decidingApprovalIds()).toEqual(['a1']);

      await vi.advanceTimersByTimeAsync(APPROVAL_DECISION_TIMEOUT_MS);
      const error = await deciding;
      expect(error?.code).toBe('timeout');
      expect(error?.message).toContain('The decision could not be confirmed. The action may already be running.');
      expect(store.decidingApprovalIds()).toEqual([]);

      late.resolve(true);
      await vi.advanceTimersByTimeAsync(0);
      expect(store.approvals().map((entry) => entry.id)).toEqual(['a1']);
      expect(resolveApproval.mock.calls).toEqual([['a1', true]]);
      expect(store.error()).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it.each([
    new IpcError('permissionDenied', 'Decision refused', 'core'),
    new IpcError('unavailable', 'Native desktop connection missing', 'notDispatched'),
  ])('preserves a confirmed decision failure from $origin', async (failure) => {
    await store.start();
    vi.spyOn(api, 'resolveApproval').mockRejectedValueOnce(failure);

    await expect(store.resolve('a1', true)).resolves.toBe(failure);
    expect(store.approvals().map((entry) => entry.id)).toEqual(['a1']);
    expect(store.decidingApprovalIds()).toEqual([]);
    expect(store.error()).toBeNull();
  });

  it('treats a transport rejection of Deny as an uncertain decision', async () => {
    await store.start();
    const failure = new IpcError('internal', 'Connection closed', 'transport');
    vi.spyOn(api, 'resolveApproval').mockRejectedValueOnce(failure);

    const result = await store.resolve('a1', false);
    expect(result).toMatchObject({ code: failure.code, origin: failure.origin });
    expect(result?.message).toContain('The decision could not be confirmed.');
    expect(result?.message).toContain(failure.message);
    expect(result?.message).not.toContain('The action may already be running.');
    expect(store.approvals().map((entry) => entry.id)).toEqual(['a1']);
  });

  it('releases waiting decisions on disconnect and protects the recovered request from stale replies', async () => {
    await store.start();
    const oldReply = deferred<boolean>();
    const currentReply = deferred<boolean>();
    const resolveApproval = vi
      .spyOn(api, 'resolveApproval')
      .mockReturnValueOnce(oldReply.promise)
      .mockReturnValueOnce(currentReply.promise);
    const oldDecision = store.resolve('a1', true);

    store.connectionLost(new IpcError('unavailable', 'Core offline.', 'core'));
    expect(store.approvals()).toEqual([]);
    expect(store.decidingApprovalIds()).toEqual([]);
    await expect(oldDecision).resolves.toMatchObject({
      message: expect.stringContaining('The decision could not be confirmed.'),
    });

    await store.start();
    expect(resolveApproval.mock.calls).toEqual([['a1', true]]);
    const currentDecision = store.resolve('a1', false);
    oldReply.resolve(true);
    await oldReply.promise;
    expect(store.approvals().map((entry) => entry.id)).toEqual(['a1']);
    expect(store.decidingApprovalIds()).toEqual(['a1']);
    expect(store.resolve('a1', true)).toBe(currentDecision);
    expect(resolveApproval.mock.calls).toEqual([
      ['a1', true],
      ['a1', false],
    ]);

    currentReply.resolve(true);
    await expect(currentDecision).resolves.toBeNull();
    expect(store.approvals()).toEqual([]);
    expect(store.decidingApprovalIds()).toEqual([]);
  });

  it('settles a pending decision on destruction and refuses further dispatch', async () => {
    await store.start();
    const late = deferred<boolean>();
    const resolveApproval = vi.spyOn(api, 'resolveApproval').mockReturnValueOnce(late.promise);
    const deciding = store.resolve('a1', true);

    TestBed.resetTestingModule();
    await expect(deciding).resolves.toMatchObject({
      code: 'cancelled',
      message: expect.stringContaining('could not be confirmed'),
    });
    expect(store.decidingApprovalIds()).toEqual([]);
    await expect(store.resolve('a1', true)).resolves.toMatchObject({ code: 'cancelled' });
    late.reject(new IpcError('internal', 'Old transport closed.', 'transport'));
    expect(resolveApproval.mock.calls).toEqual([['a1', true]]);
  });

  it('keeps a live approval event when the initial snapshot arrives late', async () => {
    let finish!: (value: PendingApproval[]) => void;
    api.listPendingApprovals.mockImplementationOnce(() => new Promise((resolve) => (finish = resolve)));
    const starting = store.start();
    await vi.waitFor(() => expect(api.listPendingApprovals).toHaveBeenCalled());
    api.onPending!([approval('new-request')]);
    finish([approval('expired-request')]);
    await starting;
    expect(store.approvals().map((a) => a.id)).toEqual(['new-request']);
  });

  it('does not restore a resolved approval from a late initial snapshot', async () => {
    let finish!: (value: PendingApproval[]) => void;
    api.listPendingApprovals.mockImplementationOnce(() => new Promise((resolve) => (finish = resolve)));
    const starting = store.start();
    await vi.waitFor(() => expect(api.listPendingApprovals).toHaveBeenCalled());
    api.onPending!([]);
    finish([approval('resolved')]);
    await starting;
    expect(store.approvals()).toEqual([]);
  });

  it('clears the old journal when opening a missing task link', async () => {
    await store.select('t1');
    api.getAgentTask.mockRejectedValueOnce(new IpcError('notFound', 'Task not found'));
    await store.select('missing');
    expect(store.selected()).toBeNull();
    expect(store.error()?.code).toBe('notFound');
  });

  it('keeps live task updates and new tasks when a list snapshot arrives late', async () => {
    await store.start();
    await store.select('t1');
    const response = deferred<TaskSummary[]>();
    api.listAgentTasks.mockReturnValueOnce(response.promise);
    const loading = store.loadTasks();
    api.onTask!(agentTask('t1', 'completed', 10));
    api.onTask!(agentTask('new', 'running', 10));
    response.resolve([summarize(agentTask('t1', 'running', 10))]);
    await loading;
    expect(store.tasks().find((t) => t.id === 't1')?.status).toBe('completed');
    expect(store.tasks().find((t) => t.id === 'new')?.status).toBe('running');
    expect(store.running()).toBe(1);
  });

  it('releases a hung task list, permits retry and ignores the late snapshot', async () => {
    vi.useFakeTimers();
    try {
      const late = deferred<TaskSummary[]>();
      api.listAgentTasks.mockReturnValueOnce(late.promise);
      const pending = store.loadAll();
      await vi.advanceTimersByTimeAsync(AGENT_READ_TIMEOUT_MS);
      await pending;
      expect(store.busy()).toBe(false);
      expect(store.error()?.code).toBe('timeout');
      await store.loadAll();
      late.resolve([summarize(agentTask('late', 'running'))]);
      await Promise.resolve();
      expect(store.tasks().map((task) => task.id)).toEqual(['t1']);
      expect(store.error()).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it('keeps a nested journal timeout visible after its parent list succeeds', async () => {
    vi.useFakeTimers();
    try {
      api.getAgentTask.mockReturnValueOnce(new Promise(() => undefined));
      const pending = store.loadTasks();
      await vi.advanceTimersByTimeAsync(AGENT_READ_TIMEOUT_MS);
      await pending;
      expect(store.tasks()).toHaveLength(1);
      expect(store.selected()).toBeNull();
      expect(store.error()?.code).toBe('timeout');
      expect(store.busy()).toBe(false);
      await store.select('t1');
      expect(store.selected()?.id).toBe('t1');
      expect(store.error()).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it('keeps a live journal when its snapshot times out', async () => {
    await store.start();
    vi.useFakeTimers();
    try {
      api.getAgentTask.mockReturnValueOnce(new Promise(() => undefined));
      const pending = store.select('t1');
      api.onTask!(agentTask('t1', 'completed', 10));
      await vi.advanceTimersByTimeAsync(AGENT_READ_TIMEOUT_MS);
      await pending;
      expect(store.selected()?.status).toBe('completed');
      expect(store.error()).toBeNull();
      expect(store.busy()).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });

  it('settles all pending task reads on destruction and blocks new reads', async () => {
    const list = deferred<TaskSummary[]>();
    const journal = deferred<AgentTask>();
    api.listAgentTasks.mockReturnValueOnce(list.promise);
    api.getAgentTask.mockReturnValueOnce(journal.promise);
    api.getAgentStatus.mockReturnValueOnce(new Promise(() => undefined));
    api.listTools.mockReturnValueOnce(new Promise(() => undefined));
    const loading = store.loadAll();
    const selecting = store.select('t1');
    TestBed.resetTestingModule();
    await Promise.all([loading, selecting]);
    list.resolve([summarize(agentTask('late', 'running'))]);
    journal.resolve(agentTask('late', 'running'));
    await store.loadAll();
    await store.select('late');
    expect(store.tasks()).toEqual([]);
    expect(store.selected()).toBeNull();
    expect(store.status()).toBeNull();
    expect(store.tools()).toEqual([]);
    expect(store.error()).toBeNull();
    expect(store.busy()).toBe(false);
    expect(api.listAgentTasks).toHaveBeenCalledTimes(1);
    expect(api.getAgentTask).toHaveBeenCalledTimes(1);
    expect(api.getAgentStatus).toHaveBeenCalledTimes(1);
    expect(api.listTools).toHaveBeenCalledTimes(1);
  });

  it('does not let sibling success hide an initial status failure', async () => {
    api.getAgentStatus.mockRejectedValueOnce(new Error('status unavailable'));
    const tasks = deferred<TaskSummary[]>();
    api.listAgentTasks.mockReturnValueOnce(tasks.promise);
    const loading = store.loadAll();
    tasks.resolve([summarize(agentTask('t1', 'completed'))]);
    await loading;
    expect(store.error()?.message).toContain('status unavailable');
    expect(store.selected()?.id).toBe('t1');
  });

  it('times out recovery snapshots and retries the connection', async () => {
    await failConnection();
    vi.useFakeTimers();
    try {
      api.listAgentTasks.mockReturnValueOnce(new Promise(() => undefined));
      const recovering = store.start();
      await vi.advanceTimersByTimeAsync(AGENT_READ_TIMEOUT_MS);
      await recovering;
      expect(store.connecting()).toBe(false);
      expect(store.subscriptionError()?.code).toBe('timeout');
      await store.start();
      expect(store.subscriptionError()).toBeNull();
      expect(store.tasks()).toHaveLength(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it('times out the approval snapshot and releases its listeners before retrying', async () => {
    const cleanup = vi.fn(() => undefined);
    api.onAgentTask.mockResolvedValueOnce(cleanup);
    api.onApprovals.mockResolvedValueOnce(cleanup);
    api.listPendingApprovals.mockReturnValueOnce(new Promise(() => undefined));
    vi.useFakeTimers();
    try {
      const starting = store.start();
      await vi.advanceTimersByTimeAsync(AGENT_READ_TIMEOUT_MS);
      await starting;
      expect(cleanup).toHaveBeenCalledTimes(2);
      expect(store.subscriptionError()?.code).toBe('timeout');
      await store.start();
      expect(store.subscriptionError()).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it.each([false, true])(
    'keeps an explicit cleared journal when a task list arrives later (previous selection: %s)',
    async (existing) => {
      if (existing) await store.select('original');
      api.getAgentTask.mockClear();
      const response = deferred<TaskSummary[]>();
      api.listAgentTasks.mockReturnValueOnce(response.promise);
      const loading = store.loadTasks();
      await store.select(null);
      response.resolve([summarize(agentTask('listed', 'completed', 10))]);
      await loading;
      expect(store.tasks().map((task) => task.id)).toEqual(['listed']);
      expect(store.selectedId()).toBeNull();
      expect(store.selected()).toBeNull();
      expect(api.getAgentTask).not.toHaveBeenCalled();
    }
  );

  it('keeps a journal selected while its task list is pending', async () => {
    const response = deferred<TaskSummary[]>();
    api.listAgentTasks.mockReturnValueOnce(response.promise);
    const loading = store.loadTasks();
    await store.select('chosen');
    const selected = store.selected();
    response.resolve([summarize(agentTask('listed', 'completed', 10))]);
    await loading;
    expect(store.selectedId()).toBe('chosen');
    expect(store.selected()).toBe(selected);
    expect(api.getAgentTask).toHaveBeenCalledExactlyOnceWith('chosen');
  });

  it('selects the newest task on an initial list load with no newer journal choice', async () => {
    api.listAgentTasks.mockResolvedValueOnce([
      summarize(agentTask('older', 'completed', 1)),
      summarize(agentTask('newer', 'completed', 2)),
    ]);
    await store.loadTasks();
    expect(store.selectedId()).toBe('newer');
    expect(store.selected()?.id).toBe('newer');
    expect(api.getAgentTask).toHaveBeenCalledExactlyOnceWith('newer');
  });

  it('ignores an older list request after a newer refresh finishes', async () => {
    await store.select('t1');
    const old = deferred<TaskSummary[]>();
    api.listAgentTasks.mockReturnValueOnce(old.promise);
    const loading = store.loadTasks();
    api.listAgentTasks.mockResolvedValueOnce([]);
    await store.loadTasks();
    old.resolve([summarize(agentTask('t1', 'running'))]);
    await loading;
    expect(store.tasks()).toEqual([]);
    expect(store.busy()).toBe(false);
  });

  it('keeps an event journal even when the snapshot has the same timestamp', async () => {
    await store.start();
    const response = deferred<AgentTask>();
    api.getAgentTask.mockReturnValueOnce(response.promise);
    const selecting = store.select('t1');
    api.onTask!(agentTask('t1', 'completed', 10));
    response.resolve(agentTask('t1', 'running', 10));
    await selecting;
    expect(store.selected()?.status).toBe('completed');
    expect(store.selected()?.result).toContain('Done:');
  });

  it('ignores a stale failure after switching away and back to the same task', async () => {
    const response = deferred<AgentTask>();
    api.getAgentTask.mockReturnValueOnce(response.promise);
    const selecting = store.select('t1');
    await store.select('t2');
    await store.select('t1');
    response.reject(new IpcError('notFound', 'Old request failed'));
    await selecting;
    expect(store.selected()?.id).toBe('t1');
    expect(store.error()).toBeNull();
    expect(store.busy()).toBe(false);
  });

  it('does not let a stale successful selection erase the current error', async () => {
    const response = deferred<AgentTask>();
    api.getAgentTask.mockReturnValueOnce(response.promise);
    const selecting = store.select('t1');
    api.getAgentTask.mockRejectedValueOnce(new IpcError('notFound', 'Current task missing'));
    await store.select('missing');
    response.resolve(agentTask('t1', 'completed'));
    await selecting;
    expect(store.selected()).toBeNull();
    expect(store.error()?.message).toBe('Current task missing');
  });

  it('ignores an old journal after reopening the same task', async () => {
    const response = deferred<AgentTask>();
    api.getAgentTask.mockReturnValueOnce(response.promise);
    const selecting = store.select('t1');
    await store.select(null);
    await store.select('t1');
    response.resolve(agentTask('t1', 'running'));
    await selecting;
    expect(store.selected()?.status).toBe('completed');
  });

  it('does not restore a cleared journal from a pending refresh', async () => {
    await store.select('t1');
    const response = deferred<AgentTask>();
    api.getAgentTask.mockReturnValueOnce(response.promise);
    const selecting = store.select('t1');
    api.clearFinishedAgentTasks.mockResolvedValueOnce(['t1']);
    api.listAgentTasks.mockResolvedValueOnce([]);
    await store.clearFinished();
    response.resolve(agentTask('t1', 'completed'));
    await selecting;
    expect(store.selectedId()).toBeNull();
    expect(store.selected()).toBeNull();
    expect(store.tasks()).toEqual([]);
  });

  it('preserves completion pushed before the start response arrives', async () => {
    await store.start();
    const response = deferred<AgentTask>();
    vi.spyOn(api, 'startAgentTask').mockReturnValueOnce(response.promise);
    const starting = store.startTask({
      goal: 'List files',
      providerId: null,
      model: null,
      maxSteps: null,
      maxDurationSecs: null,
    });
    api.onTask!(agentTask('created', 'completed', 10));
    response.resolve(agentTask('created', 'pending', 10));
    expect((await starting)?.status).toBe('completed');
    expect(store.selected()?.result).toContain('Done:');
    expect(store.tasks()[0].status).toBe('completed');
  });

  it('pages through the audit log and verifies it', async () => {
    await store.loadAudit();
    expect(store.audit()).toHaveLength(AUDIT_PAGE);
    expect(store.auditComplete()).toBe(false);
    await store.loadAudit(true);
    expect(store.audit()).toHaveLength(AUDIT_PAGE + 2);
    expect(store.auditComplete()).toBe(true);
    expect(api.auditRequests[1]).toBe(store.audit()[AUDIT_PAGE - 1].id);

    await store.verifyAudit();
    expect(store.verification()?.intact).toBe(true);
  });

  it('shares pending audit pages and stops after the last page', async () => {
    const page = deferred<AuditEntry[]>();
    const get = vi.spyOn(api, 'getAuditLog').mockReturnValueOnce(page.promise);
    const first = store.loadAudit();
    expect(store.loadAudit()).toBe(first);
    expect(store.loadAudit(true)).toBe(first);
    expect(store.auditLoading()).toBe(true);
    page.resolve([auditEntry('one', 1)]);
    await first;
    await store.loadAudit(true);
    expect(get).toHaveBeenCalledTimes(1);
    expect(store.auditLoading()).toBe(false);
    await store.loadAudit();
    expect(get).toHaveBeenCalledTimes(2);
  });

  it('coalesces integrity checks without blocking other agent operations', async () => {
    const result = deferred<AuditVerification>();
    api.verifyAuditLog.mockReturnValueOnce(result.promise);
    const first = store.verifyAudit();
    expect(store.verifyAudit()).toBe(first);
    expect(api.verifyAuditLog).toHaveBeenCalledTimes(1);
    expect(store.verifyingAudit()).toBe(true);
    expect(store.busy()).toBe(false);
    await store.loadAudit();
    expect(store.verifyingAudit()).toBe(true);
    result.resolve({ entries: 4, intact: false, brokenAt: 'broken' });
    await first;
    expect(store.verification()?.brokenAt).toBe('broken');
    expect(store.verifyingAudit()).toBe(false);
  });

  it('clears the old verdict on recheck and preserves failures across unrelated requests', async () => {
    await store.verifyAudit();
    expect(store.verification()?.intact).toBe(true);
    api.verifyAuditLog.mockRejectedValueOnce(new Error('verification unavailable'));
    const retry = store.verifyAudit();
    expect(store.verification()).toBeNull();
    await retry;
    await store.loadAll();
    await store.loadAudit();
    expect(store.verificationError()?.message).toContain('verification unavailable');
    expect(store.verification()).toBeNull();
    expect(store.verifyingAudit()).toBe(false);
    await store.verifyAudit();
    expect(store.verificationError()).toBeNull();
    expect(store.verification()?.intact).toBe(true);
  });

  it.each(['resolve', 'reject'])('ignores a late integrity %s after timeout and retry', async (outcome) => {
    vi.useFakeTimers();
    try {
      const late = deferred<AuditVerification>();
      api.verifyAuditLog.mockReturnValueOnce(late.promise);
      const first = store.verifyAudit();
      await vi.advanceTimersByTimeAsync(AUDIT_TIMEOUT_MS);
      await first;
      expect(store.verificationError()?.code).toBe('timeout');
      expect(store.verifyingAudit()).toBe(false);
      await store.verifyAudit();
      if (outcome === 'resolve') late.resolve({ entries: 1, intact: false, brokenAt: 'stale' });
      else late.reject(new Error('late failure'));
      await Promise.resolve();
      expect(store.verification()?.intact).toBe(true);
      expect(store.verificationError()).toBeNull();
      expect(api.verifyAuditLog).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it('settles integrity checks on destruction and ignores their late result', async () => {
    const result = deferred<AuditVerification>();
    api.verifyAuditLog.mockReturnValueOnce(result.promise);
    const pending = store.verifyAudit();
    TestBed.resetTestingModule();
    await pending;
    result.resolve({ entries: 1, intact: true, brokenAt: null });
    await store.verifyAudit();
    expect(api.verifyAuditLog).toHaveBeenCalledTimes(1);
    expect(store.verification()).toBeNull();
    expect(store.verificationError()).toBeNull();
    expect(store.verifyingAudit()).toBe(false);
  });

  it.each(['success', 'failure'])('ignores a superseded older audit page: %s', async (outcome) => {
    await store.loadAudit();
    const older = deferred<AuditEntry[]>();
    const fresh = deferred<AuditEntry[]>();
    vi.spyOn(api, 'getAuditLog').mockReturnValueOnce(older.promise).mockReturnValueOnce(fresh.promise);
    const more = store.loadAudit(true);
    expect(store.loadAudit(true)).toBe(more);
    const refresh = store.loadAudit();
    if (outcome === 'success') older.resolve([auditEntry('stale', 0)]);
    else older.reject(new Error('stale failure'));
    await more;
    expect(store.auditLoading()).toBe(true);
    expect(store.auditError()).toBeNull();
    fresh.resolve([auditEntry('fresh', 10)]);
    await refresh;
    expect(store.audit().map((entry) => entry.id)).toEqual(['fresh']);
    expect(store.auditComplete()).toBe(true);
    expect(store.auditLoading()).toBe(false);
  });

  it('preserves audit data and its error across unrelated successful requests', async () => {
    await store.loadAudit();
    vi.spyOn(api, 'getAuditLog').mockRejectedValueOnce(new Error('audit unavailable'));
    await store.loadAudit(true);
    await store.refreshStatus();
    expect(store.audit()).toHaveLength(AUDIT_PAGE);
    expect(store.auditError()?.message).toContain('audit unavailable');
    await store.loadAudit(true);
    expect(store.audit()).toHaveLength(AUDIT_PAGE + 2);
    expect(store.auditError()).toBeNull();
  });

  it('deduplicates overlapping audit rows', async () => {
    await store.loadAudit();
    vi.spyOn(api, 'getAuditLog').mockResolvedValueOnce([store.audit()[0], auditEntry('new', 0)]);
    await store.loadAudit(true);
    expect(store.audit()).toHaveLength(AUDIT_PAGE + 1);
    expect(new Set(store.audit().map((entry) => entry.id)).size).toBe(AUDIT_PAGE + 1);
  });

  it('times out audit reads, permits retry and ignores late responses', async () => {
    vi.useFakeTimers();
    try {
      const stale = deferred<AuditEntry[]>();
      vi.spyOn(api, 'getAuditLog').mockReturnValueOnce(stale.promise);
      const pending = store.loadAudit();
      await vi.advanceTimersByTimeAsync(AUDIT_TIMEOUT_MS);
      await pending;
      expect(store.auditLoading()).toBe(false);
      expect(store.auditError()?.code).toBe('timeout');
      await store.loadAudit();
      stale.resolve([auditEntry('late', 0)]);
      await Promise.resolve();
      expect(store.audit()).toHaveLength(AUDIT_PAGE);
      expect(store.auditError()).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it('settles audit reads on destruction without applying late data or starting new reads', async () => {
    const page = deferred<AuditEntry[]>();
    const get = vi.spyOn(api, 'getAuditLog').mockReturnValueOnce(page.promise);
    const pending = store.loadAudit();
    TestBed.resetTestingModule();
    await pending;
    page.resolve([auditEntry('late', 0)]);
    await store.loadAudit();
    expect(get).toHaveBeenCalledTimes(1);
    expect(store.audit()).toEqual([]);
    expect(store.auditError()).toBeNull();
    expect(store.auditLoading()).toBe(false);
  });

  it('applies settings returned by the folder dialog', async () => {
    const settings = TestBed.inject(SettingsStore);
    settings.settings.set(SETTINGS);
    await store.addWorkspaceFolder();
    expect(settings.settings()?.tools.workspaceFolders).toEqual(['/new']);
    expect(api.getAgentStatus).toHaveBeenCalled();
  });

  it('forgets a confirmed deleted selection without looking up the removed journal', async () => {
    await store.loadTasks();
    expect(store.selectedId()).toBe('t1');
    api.clearFinishedAgentTasks.mockResolvedValueOnce(['t1']);
    api.listAgentTasks.mockResolvedValueOnce([]);
    await store.clearFinished();
    expect(store.selected()).toBeNull();
    expect(store.tasks()).toEqual([]);
    expect(api.getAgentTask).toHaveBeenCalledExactlyOnceWith('t1');
  });

  it('keeps a newer selection that survives a delayed bulk clear outside the capped list', async () => {
    await store.loadTasks();
    const acknowledgement = deferred<string[]>();
    api.clearFinishedAgentTasks.mockReturnValueOnce(acknowledgement.promise);
    const clearing = store.clearFinished();

    const survivor = agentTask('survivor', 'completed', 1);
    api.getAgentTask.mockResolvedValue(survivor);
    await store.select(survivor.id);
    const capped = Array.from({ length: 100 }, (_, index) => summarize(agentTask(`task-${index}`, 'running', index + 2)));
    api.listAgentTasks.mockResolvedValueOnce(capped);
    acknowledgement.resolve(['t1']);
    await clearing;

    expect(store.tasks()).toHaveLength(100);
    expect(store.selectedId()).toBe(survivor.id);
    expect(store.selected()).toBe(survivor);
  });

  it('evicts all confirmed bulk deletions before refresh and ignores stale events and snapshots for them', async () => {
    await store.start();
    const survivor = agentTask('survivor', 'running', 10);
    const removed = [agentTask('old-a', 'completed', 5), agentTask('old-b', 'cancelled', 4)];
    const oldList = [survivor, ...removed].map(summarize);
    api.listAgentTasks.mockResolvedValueOnce(oldList);
    api.getAgentTask.mockResolvedValue(survivor);
    await store.loadTasks();

    const reload = deferred<TaskSummary[]>();
    api.listAgentTasks.mockReturnValueOnce(reload.promise);
    api.clearFinishedAgentTasks.mockResolvedValueOnce(removed.map((task) => task.id));
    const clearing = store.clearFinished();
    await vi.waitFor(() => expect(api.listAgentTasks).toHaveBeenCalledTimes(2));

    expect(store.tasks()).toEqual([summarize(survivor)]);
    expect(store.selected()).toBe(survivor);
    for (const task of removed) api.onTask!(task);
    expect(store.tasks()).toEqual([summarize(survivor)]);

    reload.reject(new IpcError('io', 'Task list reload failed', 'core'));
    await clearing;
    expect(store.error()?.message).toContain('Task list reload failed');
    expect(store.tasks()).toEqual([summarize(survivor)]);

    api.listAgentTasks.mockResolvedValueOnce(oldList);
    await store.loadTasks();
    expect(store.tasks()).toEqual([summarize(survivor)]);
  });

  it('refreshes the open journal and clears it only after an exact core notFound', async () => {
    await store.loadTasks();
    const opened = store.selected();
    api.listAgentTasks.mockResolvedValue([]);
    api.getAgentTask.mockRejectedValueOnce(new IpcError('notFound', 'Transport reply unavailable', 'transport'));

    await store.refreshTasks();

    expect(store.tasks()).toEqual([]);
    expect(store.selectedId()).toBe('t1');
    expect(store.selected()).toBe(opened);

    api.getAgentTask.mockRejectedValueOnce(new IpcError('notFound', 'Deleted', 'core'));
    await store.refreshTasks();

    expect(api.getAgentTask).toHaveBeenLastCalledWith('t1');
    expect(store.selectedId()).toBeNull();
    expect(store.selected()).toBeNull();
  });

  it('releases timed-out cleanup without replaying or applying the late acknowledgement', async () => {
    vi.useFakeTimers();
    try {
      await store.loadTasks();
      const opened = store.selected();
      const acknowledgement = deferred<number>();
      api.deleteReviewedAgentTask.mockReturnValueOnce(acknowledgement.promise);
      const deleting = store.deleteReviewed('t1');
      const overlapping = store.clearFinished();
      expect(store.busy()).toBe(true);
      expect(api.clearFinishedAgentTasks).not.toHaveBeenCalled();

      await vi.advanceTimersByTimeAsync(AGENT_COMMAND_TIMEOUT_MS);
      await Promise.all([deleting, overlapping]);
      const warning = store.error();
      expect(store.busy()).toBe(false);
      expect(warning?.code).toBe('timeout');
      expect(warning?.message).toContain('Refresh tasks');

      acknowledgement.resolve(1);
      await Promise.resolve();
      expect(store.error()).toBe(warning);
      expect(store.selected()).toBe(opened);
      expect(store.tasks().map((entry) => entry.id)).toEqual(['t1']);
      expect(api.deleteReviewedAgentTask).toHaveBeenCalledExactlyOnceWith('t1');
      expect(api.listAgentTasks).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it.each(['unknown', 'inProgress'] as const)('keeps a selected journal with an %s action after bulk clear', async (outcome) => {
    const task = agentTask(`retained-${outcome}`, 'cancelled');
    task.steps[0].outcome = outcome;
    api.listAgentTasks.mockResolvedValue([summarize(task)]);
    api.getAgentTask.mockResolvedValue(task);
    await store.loadTasks();

    await store.clearFinished();

    expect(api.clearFinishedAgentTasks).toHaveBeenCalledOnce();
    expect(store.selectedId()).toBe(task.id);
    expect(store.selected()).toBe(task);
    expect(store.tasks().map((entry) => entry.id)).toEqual([task.id]);
  });

  it('deletes only the reviewed journal and retains the other unresolved task', async () => {
    const reviewed = agentTask('reviewed', 'cancelled', 20);
    reviewed.steps[0].outcome = 'unknown';
    const retained = agentTask('retained', 'failed', 10);
    retained.steps[0].outcome = 'unknown';
    api.listAgentTasks.mockResolvedValueOnce([summarize(reviewed), summarize(retained)]);
    api.listAgentTasks.mockResolvedValueOnce([summarize(retained)]);
    api.getAgentTask.mockImplementation(async (id: string) => (id === reviewed.id ? reviewed : retained));
    await store.loadTasks();
    expect(store.selectedId()).toBe(reviewed.id);

    await store.deleteReviewed(reviewed.id);

    expect(api.deleteReviewedAgentTask).toHaveBeenCalledOnce();
    expect(api.deleteReviewedAgentTask).toHaveBeenCalledWith(reviewed.id);
    expect(store.tasks().map((entry) => entry.id)).toEqual([retained.id]);
    expect(store.selectedId()).toBe(retained.id);
    expect(store.selected()).toBe(retained);
  });

  it('evicts a confirmed reviewed deletion even when the list reload fails', async () => {
    await store.start();
    await store.loadTasks();
    const removed = store.selected()!;
    const stale = deferred<TaskSummary[]>();
    api.listAgentTasks.mockReturnValueOnce(stale.promise);
    const listing = store.loadTasks();
    api.listAgentTasks.mockRejectedValueOnce(new IpcError('io', 'Task list reload failed', 'core'));

    await store.deleteReviewed(removed.id);

    expect(store.error()?.message).toContain('Task list reload failed');
    expect(store.tasks()).toEqual([]);
    expect(store.selectedId()).toBeNull();
    expect(store.selected()).toBeNull();

    stale.resolve([summarize(removed)]);
    await listing;
    api.onTask!(removed);
    expect(store.tasks()).toEqual([]);
    expect(store.selected()).toBeNull();
  });

  it('reconciles a zero reviewed deletion count without assuming the journal was removed', async () => {
    await store.loadTasks();
    api.deleteReviewedAgentTask.mockResolvedValueOnce(0);

    await store.deleteReviewed('t1');

    expect(store.selectedId()).toBe('t1');
    expect(store.selected()?.id).toBe('t1');
    expect(store.tasks().map((entry) => entry.id)).toEqual(['t1']);
  });
});

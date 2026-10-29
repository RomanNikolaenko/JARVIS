import { DestroyRef, Injectable, computed, inject, signal } from '@angular/core';
import type {
  AgentStartRequest,
  AgentStatus,
  AgentTask,
  AuditEntry,
  AuditVerification,
  PendingApproval,
  TaskStatus,
  TaskSummary,
  ToolInfo,
} from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';
import { withRequestDeadline } from '../ipc/request-deadline';
import { SettingsStore } from './settings.store';

export const AUDIT_PAGE = 100;
export const AUDIT_TIMEOUT_MS = 30_000;
export const AGENT_READ_TIMEOUT_MS = 30_000;
export const MAX_GOAL_LENGTH = 4000;

const FINISHED: readonly TaskStatus[] = ['completed', 'unverified', 'failed', 'cancelled'];

export function isFinished(status: TaskStatus): boolean {
  return FINISHED.includes(status);
}

/** Mirrors `TaskSummary::from(&AgentTask)` in the Rust core. */
export function summarize(task: AgentTask): TaskSummary {
  return {
    id: task.id,
    goal: task.goal,
    status: task.status,
    model: task.model,
    stepCount: task.steps.length,
    error: task.error,
    createdAtMs: task.createdAtMs,
    updatedAtMs: task.updatedAtMs,
  };
}

/** Agent tasks, pending approvals, tools and the audit log. */
@Injectable({ providedIn: 'root' })
export class AgentsStore {
  private readonly api = inject(JarvisApi);
  private readonly destroyRef = inject(DestroyRef);
  private auditEpoch = 0;
  private auditRead: { more: boolean; promise: Promise<void> } | null = null;
  private verificationRequest: Promise<void> | null = null;
  private readonly settingsStore = inject(SettingsStore);
  private started = false;
  private recoveryNeeded = false;
  private destroyed = false;
  private starting: Promise<void> | null = null;
  private disconnect: (() => void) | null = null;
  private pending = 0;
  private selectionRequest = 0;
  private listRequest = 0;
  private statusRequest = 0;
  private toolsRequest = 0;
  // Keep full event payloads only while an IPC snapshot is in flight.
  private readonly taskReads = new Set<Map<string, AgentTask>>();

  readonly status = signal<AgentStatus | null>(null);
  readonly tools = signal<readonly ToolInfo[]>([]);
  readonly tasks = signal<readonly TaskSummary[]>([]);
  readonly selectedId = signal<string | null>(null);
  readonly selected = signal<AgentTask | null>(null);
  readonly approvals = signal<readonly PendingApproval[]>([]);
  readonly audit = signal<readonly AuditEntry[]>([]);
  readonly auditComplete = signal(false);
  readonly auditLoading = signal(false);
  readonly auditError = signal<IpcError | null>(null);
  readonly verification = signal<AuditVerification | null>(null);
  readonly verifyingAudit = signal(false);
  readonly verificationError = signal<IpcError | null>(null);
  readonly error = signal<IpcError | null>(null);
  readonly busy = signal(false);
  readonly connecting = signal(false);
  readonly subscriptionError = signal<IpcError | null>(null);

  constructor() {
    inject(DestroyRef).onDestroy(() => {
      this.destroyed = true;
      this.disconnect?.();
    });
  }

  readonly running = computed(() => this.tasks().filter((t) => !isFinished(t.status)).length);
  readonly hasFinished = computed(() => this.tasks().some((t) => isFinished(t.status)));

  /**
   * Subscribes to task updates and approval requests (app-wide, so an
   * approval request reaches the user on every screen).
   */
  start(): Promise<void> {
    if (this.destroyed || this.started) return Promise.resolve();
    if (this.starting) return this.starting;
    this.connecting.set(true);
    const attempt = this.subscribe().finally(() => {
      if (this.starting === attempt) {
        this.starting = null;
        this.connecting.set(false);
      }
    });
    this.starting = attempt;
    return attempt;
  }

  /** Called after a failed health probe; never cancels or restarts native tasks. */
  connectionLost(error: IpcError): void {
    if (this.destroyed) return;
    this.started = false;
    this.recoveryNeeded = true;
    this.disconnect?.();
    this.disconnect = null;
    // An unavailable transport may never settle its old registration promise.
    this.starting = null;
    this.connecting.set(false);
    this.subscriptionError.set(error);
    this.approvals.set([]);
  }

  private async subscribe(): Promise<void> {
    let active = true;
    const listeners: (() => void)[] = [];
    const dispose = () => {
      active = false;
      for (const unlisten of listeners.splice(0)) unlisten();
    };
    this.disconnect = dispose;
    const retain = (unlisten: () => void): boolean => {
      if (!active) {
        unlisten();
        return false;
      }
      listeners.push(unlisten);
      return true;
    };
    // Retain inside the native promise so a registration that finishes after
    // the deadline still releases its listener when this attempt is inactive.
    const register = (request: () => Promise<() => void>, label: string) => this.read(() => request().then(retain), label);
    try {
      if (
        !(await register(
          () =>
            this.api.onAgentTask((task) => {
              if (active) this.merge(task);
            }),
          'Agent task subscription'
        ))
      )
        return;
      let receivedApprovalEvent = false;
      if (
        !(await register(
          () =>
            this.api.onApprovals((approvals) => {
              if (!active) return;
              receivedApprovalEvent = true;
              this.approvals.set(approvals);
            }),
          'Approval subscription'
        ))
      )
        return;
      const approvals = await this.read(() => this.api.listPendingApprovals(), 'Pending approvals');
      if (!active) return;
      if (!receivedApprovalEvent) this.approvals.set(approvals);
      if (this.recoveryNeeded) await this.recoverTasks(() => active);
      if (!active) return;
      this.recoveryNeeded = false;
      this.started = true;
      this.subscriptionError.set(null);
    } catch (error) {
      if (active) {
        this.recoveryNeeded = true;
        this.subscriptionError.set(IpcError.from(error));
      }
      dispose();
    }
  }

  /** Reconcile changes missed while listeners were disconnected. */
  private recoverTasks(isActive: () => boolean): Promise<void> {
    const listRequest = ++this.listRequest;
    const selectionRequest = ++this.selectionRequest;
    const id = this.selectedId();
    return this.observeTasks(async (updates) => {
      const journal =
        id === null
          ? Promise.resolve(null)
          : this.read(() => this.api.getAgentTask(id), 'Agent task').catch((error: unknown) => {
              if (IpcError.from(error).code === 'notFound') return null;
              throw error;
            });
      const [tasks, task] = await Promise.all([this.read(() => this.api.listAgentTasks(100), 'Agent tasks'), journal]);
      if (!isActive()) return;
      if (listRequest === this.listRequest) {
        const merged = new Map(tasks.map((entry) => [entry.id, entry]));
        for (const update of updates.values()) merged.set(update.id, summarize(update));
        this.tasks.set([...merged.values()].sort((a, b) => b.updatedAtMs - a.updatedAtMs));
      }
      if (selectionRequest === this.selectionRequest && id !== null) {
        const current = updates.get(id) ?? task;
        this.selected.set(current);
        if (!current) this.selectedId.set(null);
      }
    });
  }

  async loadAll(): Promise<void> {
    await Promise.all([this.refreshStatus(), this.loadTools(), this.loadTasks()]);
  }

  refreshStatus(): Promise<void> {
    const request = ++this.statusRequest;
    const current = () => request === this.statusRequest && !this.destroyed;
    return this.run(async () => {
      const status = await this.read(() => this.api.getAgentStatus(), 'Agent status');
      if (current()) this.status.set(status);
    }, current);
  }

  loadTools(): Promise<void> {
    const request = ++this.toolsRequest;
    const current = () => request === this.toolsRequest && !this.destroyed;
    return this.run(async () => {
      const tools = await this.read(() => this.api.listTools(), 'Agent tools');
      if (current()) this.tools.set(tools);
    }, current);
  }

  loadTasks(): Promise<void> {
    if (this.destroyed) return Promise.resolve();
    const request = ++this.listRequest;
    return this.observeTasks((updates) =>
      this.run(
        async () => {
          const tasks = await this.read(() => this.api.listAgentTasks(100), 'Agent tasks');
          if (request !== this.listRequest || this.destroyed) return;
          const merged = new Map(tasks.map((task) => [task.id, task]));
          for (const task of updates.values()) merged.set(task.id, summarize(task));
          this.tasks.set([...merged.values()].sort((a, b) => b.updatedAtMs - a.updatedAtMs));
          const selected = this.selectedId();
          if (selected === null && this.tasks().length) {
            await this.select(this.tasks()[0].id);
          }
        },
        () => request === this.listRequest
      )
    );
  }

  select(id: string | null): Promise<void> {
    if (this.destroyed) return Promise.resolve();
    const request = ++this.selectionRequest;
    if (id !== this.selectedId()) this.selected.set(null);
    this.selectedId.set(id);
    if (id === null) {
      this.selected.set(null);
      return Promise.resolve();
    }
    return this.observeTasks((updates) =>
      this.run(
        async () => {
          const task = await this.read(() => this.api.getAgentTask(id), 'Agent task');
          if (request === this.selectionRequest && !this.destroyed) {
            this.selected.set(updates.get(id) ?? task);
          }
        },
        () => request === this.selectionRequest && !updates.has(id)
      )
    );
  }

  async startTask(request: AgentStartRequest): Promise<AgentTask | null> {
    let task: AgentTask | null = null;
    await this.observeTasks((updates) =>
      this.run(async () => {
        const created = await this.api.startAgentTask(request);
        task = updates.get(created.id) ?? created;
        ++this.selectionRequest;
        this.selectedId.set(task.id);
        this.merge(task);
      })
    );
    return task;
  }

  cancel(id: string): Promise<void> {
    return this.run(async () => {
      await this.api.cancelAgentTask(id);
    });
  }

  pause(id: string): Promise<void> {
    return this.run(async () => {
      await this.api.pauseAgentTask(id);
    });
  }

  resume(id: string): Promise<void> {
    return this.run(async () => {
      await this.api.resumeAgentTask(id);
    });
  }

  clearFinished(): Promise<void> {
    return this.run(async () => {
      await this.api.clearFinishedAgentTasks();
      const selected = this.selected();
      if (selected && isFinished(selected.status)) {
        await this.select(null);
      }
      await this.loadTasks();
    });
  }

  /** The user's decision. A stale request (already decided) is ignored. */
  async resolve(id: string, approved: boolean): Promise<IpcError | null> {
    try {
      await this.api.resolveApproval(id, approved);
      this.approvals.update((list) => list.filter((a) => a.id !== id));
      return null;
    } catch (error) {
      // Keep failures local to the dialog; unrelated background refreshes
      // must not erase the user's decision error.
      return IpcError.from(error);
    }
  }

  loadAudit(more = false): Promise<void> {
    if (this.destroyed) return Promise.resolve();
    if (this.auditRead && (more || !this.auditRead.more)) return this.auditRead.promise;
    if (more && this.auditComplete()) return Promise.resolve();
    const epoch = ++this.auditEpoch;
    const isCurrent = () => epoch === this.auditEpoch && !this.destroyed;
    const current = more ? this.audit() : [];
    const before = current.length ? current[current.length - 1].id : null;
    this.auditLoading.set(true);
    this.auditError.set(null);
    const promise = withRequestDeadline(
      () => this.api.getAuditLog(AUDIT_PAGE, before),
      AUDIT_TIMEOUT_MS,
      'Audit log',
      this.destroyRef
    )
      .then((page) => {
        if (!isCurrent()) return;
        this.audit.set([...new Map([...current, ...page].map((entry) => [entry.id, entry])).values()]);
        this.auditComplete.set(page.length < AUDIT_PAGE);
      })
      .catch((error: unknown) => {
        if (isCurrent()) this.auditError.set(IpcError.from(error));
      })
      .finally(() => {
        if (epoch === this.auditEpoch) {
          this.auditRead = null;
          this.auditLoading.set(false);
        }
      });
    this.auditRead = { more, promise };
    return promise;
  }

  verifyAudit(): Promise<void> {
    if (this.destroyed) return Promise.resolve();
    if (this.verificationRequest) return this.verificationRequest;
    this.verifyingAudit.set(true);
    this.verificationError.set(null);
    // A previous verdict must not masquerade as the result of a failed recheck.
    this.verification.set(null);
    const request = withRequestDeadline(
      () => this.api.verifyAuditLog(),
      AUDIT_TIMEOUT_MS,
      'Audit integrity check',
      this.destroyRef
    )
      .then((result) => {
        if (!this.destroyed) this.verification.set(result);
      })
      .catch((error: unknown) => {
        if (!this.destroyed) this.verificationError.set(IpcError.from(error));
      })
      .finally(() => {
        this.verificationRequest = null;
        this.verifyingAudit.set(false);
      });
    this.verificationRequest = request;
    return request;
  }

  /** Opens the folder dialog in the core and adds the chosen folder. */
  async addWorkspaceFolder(): Promise<boolean> {
    if (this.settingsStore.saving()) return false;
    let added = false;
    await this.run(async () => {
      added = await this.settingsStore.addWorkspaceFolder();
      if (added) await this.refreshStatus();
    });
    return added;
  }

  /** Applies a pushed task to the list and the open task. */
  private merge(task: AgentTask): void {
    for (const updates of this.taskReads) updates.set(task.id, task);
    const summary = summarize(task);
    this.tasks.update((list) => {
      const rest = list.filter((t) => t.id !== task.id);
      return [summary, ...rest].sort((a, b) => b.updatedAtMs - a.updatedAtMs);
    });
    if (this.selectedId() === task.id) {
      this.selected.set(task);
    }
  }

  private async observeTasks<T>(action: (updates: Map<string, AgentTask>) => Promise<T>): Promise<T> {
    const updates = new Map<string, AgentTask>();
    this.taskReads.add(updates);
    try {
      return await action(updates);
    } finally {
      this.taskReads.delete(updates);
    }
  }

  private read<T>(request: () => Promise<T>, label: string): Promise<T> {
    return withRequestDeadline(request, AGENT_READ_TIMEOUT_MS, label, this.destroyRef);
  }

  private async run(action: () => Promise<void>, isCurrent: () => boolean = () => true): Promise<void> {
    if (this.destroyed) return;
    // Clear at the start of a new batch; sibling or parent success must not
    // erase a failure from another operation in the same batch.
    if (this.pending === 0) this.error.set(null);
    this.pending++;
    this.busy.set(true);
    try {
      await action();
    } catch (error) {
      if (isCurrent() && !this.destroyed) this.error.set(IpcError.from(error));
    } finally {
      this.pending--;
      this.busy.set(this.pending > 0);
    }
  }
}

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
export const AGENT_COMMAND_TIMEOUT_MS = 30_000;
export const APPROVAL_DECISION_TIMEOUT_MS = 30_000;
export const MAX_GOAL_LENGTH = 4000;

type TaskControl = 'pause' | 'resume' | 'cancel';

interface ApprovalDecision {
  readonly promise: Promise<IpcError | null>;
  readonly interrupt: (error: IpcError) => void;
}

const FINISHED: readonly TaskStatus[] = ['completed', 'unverified', 'failed', 'cancelled'];

export function isFinished(status: TaskStatus): boolean {
  return FINISHED.includes(status);
}

export function hasUnresolvedAction(task: Pick<AgentTask, 'steps'>): boolean {
  return task.steps.some((step) => step.outcome === 'unknown' || step.outcome === 'inProgress');
}

function isMissingTask(error: unknown): boolean {
  const failure = IpcError.from(error);
  return failure.origin === 'core' && failure.code === 'notFound';
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
  private readonly selectionRequest = signal(0);
  private startRequest = 0;
  private listRequest = 0;
  private statusRequest = 0;
  private toolsRequest = 0;
  private readonly approvalDecisions = new Map<string, ApprovalDecision>();
  private readonly taskControls = signal<ReadonlyMap<string, Promise<void>>>(new Map());
  private cleanupRequest: Promise<void> | null = null;
  // Verified deletions beat queued events and snapshots dispatched before removal.
  private readonly deletedTaskIds = new Set<string>();
  // Keep full event payloads only while an IPC snapshot is in flight.
  private readonly taskReads = new Set<Map<string, AgentTask>>();
  // Creation acknowledgements belong in lists, but are not live journal events.
  private readonly taskListReads = new Set<Map<string, AgentTask>>();

  readonly status = signal<AgentStatus | null>(null);
  readonly tools = signal<readonly ToolInfo[]>([]);
  readonly tasks = signal<readonly TaskSummary[]>([]);
  readonly selectedId = signal<string | null>(null);
  readonly selected = signal<AgentTask | null>(null);
  readonly selectionVersion = this.selectionRequest.asReadonly();
  readonly approvals = signal<readonly PendingApproval[]>([]);
  readonly decidingApprovalIds = signal<readonly string[]>([]);
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
    this.destroyRef.onDestroy(() => {
      this.destroyed = true;
      this.withdrawApprovals(new IpcError('cancelled', 'Permission decisions were interrupted.'));
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
    this.withdrawApprovals(error);
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
        const failure = IpcError.from(error);
        this.recoveryNeeded = true;
        this.subscriptionError.set(failure);
        this.withdrawApprovals(failure);
      }
      dispose();
    }
  }

  /** Reconcile changes missed while listeners were disconnected. */
  private recoverTasks(isActive: () => boolean): Promise<void> {
    const listRequest = ++this.listRequest;
    const selectionRequest = this.nextSelectionRequest();
    const id = this.selectedId();
    const listUpdates = new Map<string, AgentTask>();
    this.taskListReads.add(listUpdates);
    return this.observeTasks(async (updates) => {
      try {
        const journal = id === null ? Promise.resolve(null) : this.readTaskJournal(id);
        const [tasks, task] = await Promise.all([this.read(() => this.api.listAgentTasks(100), 'Agent tasks'), journal]);
        if (!isActive()) return;
        if (listRequest === this.listRequest) {
          this.publishTaskList(tasks, listUpdates);
        }
        if (selectionRequest === this.selectionRequest() && id !== null) {
          this.publishTaskJournal(id, task, updates);
        }
      } finally {
        this.taskListReads.delete(listUpdates);
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
    const selection = this.selectionRequest();
    return this.observeTasks(
      (updates) =>
        this.run(
          async () => {
            const tasks = await this.read(() => this.api.listAgentTasks(100), 'Agent tasks');
            if (request !== this.listRequest || this.destroyed) return;
            this.publishTaskList(tasks, updates);
            const selected = this.selectedId();
            if (selection === this.selectionRequest() && selected === null && this.tasks().length) {
              await this.select(this.tasks()[0].id);
            }
          },
          () => request === this.listRequest
        ),
      true
    );
  }

  select(id: string | null): Promise<void> {
    if (this.destroyed || (id !== null && this.deletedTaskIds.has(id))) return Promise.resolve();
    const request = this.nextSelectionRequest();
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
          if (request === this.selectionRequest() && !this.destroyed) {
            this.selected.set(updates.get(id) ?? task);
          }
        },
        () => request === this.selectionRequest() && !updates.has(id)
      )
    );
  }

  async refreshTasks(): Promise<void> {
    if (this.destroyed) return;
    const id = this.selectedId();
    const journal = id === null ? Promise.resolve() : this.refreshSelectedTask(id);
    await Promise.all([this.loadTasks(), journal]);
  }

  private refreshSelectedTask(id: string): Promise<void> {
    const selection = this.nextSelectionRequest();
    const current = () => selection === this.selectionRequest() && !this.destroyed;
    return this.observeTasks((updates) =>
      this.run(
        async () => {
          const task = await this.readTaskJournal(id);
          if (current()) this.publishTaskJournal(id, task, updates);
        },
        () => current() && !updates.has(id)
      )
    );
  }

  private nextSelectionRequest(): number {
    this.selectionRequest.update((version) => version + 1);
    return this.selectionRequest();
  }

  async startTask(request: AgentStartRequest, shouldSelect?: () => boolean): Promise<AgentTask | null> {
    if (this.destroyed) return null;
    const selection = this.selectionRequest();
    const start = ++this.startRequest;
    let task: AgentTask | null = null;
    await this.observeTasks((updates) =>
      this.run(async () => {
        const created = await this.taskCommand(
          () => this.api.startAgentTask(request),
          'Task creation',
          'Task creation could not be confirmed. The task may already be running or may start later. Use Refresh tasks in Agents to check before starting another task.'
        );
        if (this.destroyed || this.deletedTaskIds.has(created.id)) return;
        const selected = selection !== this.selectionRequest() && this.selectedId() === created.id ? this.selected() : null;
        task = selected?.id === created.id ? selected : (updates.get(created.id) ?? created);
        // Creation is independent of the originating editor. Retain the
        // confirmed task even if the user has moved to another journal.
        this.merge(task, false);
        if (start === this.startRequest && selection === this.selectionRequest() && (shouldSelect?.() ?? true)) {
          this.nextSelectionRequest();
          this.selectedId.set(task.id);
          this.selected.set(task);
        }
      })
    );
    return this.destroyed ? null : task;
  }

  cancel(id: string): Promise<void> {
    return this.controlTask('cancel', id, () => this.api.cancelAgentTask(id));
  }

  pause(id: string): Promise<void> {
    return this.controlTask('pause', id, () => this.api.pauseAgentTask(id));
  }

  resume(id: string): Promise<void> {
    return this.controlTask('resume', id, () => this.api.resumeAgentTask(id));
  }

  isControlling(control: TaskControl, id: string): boolean {
    return this.taskControls().has(`${control}:${id}`);
  }

  private controlTask(control: TaskControl, id: string, request: () => Promise<boolean | void>): Promise<void> {
    if (this.destroyed) return Promise.resolve();
    const key = `${control}:${id}`;
    const pending = this.taskControls().get(key);
    if (pending) return pending;
    const label = { pause: 'Pause request', resume: 'Resume request', cancel: 'Cancel request' }[control];
    const command = this.run(async () => {
      const accepted = await this.taskCommand(
        request,
        label,
        `The ${control} request could not be confirmed. The task may still change state. Check its journal before trying again.`
      );
      if (accepted === false) {
        throw new IpcError('unavailable', `This task is no longer accepting ${control} requests. Check the task journal.`);
      }
    }).finally(() => {
      this.taskControls.update((controls) => {
        const next = new Map(controls);
        next.delete(key);
        return next;
      });
    });
    this.taskControls.update((controls) => new Map(controls).set(key, command));
    return command;
  }

  private async taskCommand<T>(request: () => Promise<T>, label: string, uncertain: string): Promise<T> {
    try {
      return await withRequestDeadline(request, AGENT_COMMAND_TIMEOUT_MS, label, this.destroyRef);
    } catch (error) {
      const failure = IpcError.from(error);
      if (
        failure.origin === 'notDispatched' ||
        (failure.origin === 'core' && failure.code !== 'io' && failure.code !== 'internal')
      )
        throw failure;
      throw new IpcError(failure.code, `${failure.message} ${uncertain}`, failure.origin);
    }
  }

  clearFinished(): Promise<void> {
    return this.cleanup(async () => {
      const removed = await this.taskCommand(
        () => this.api.clearFinishedAgentTasks(),
        'Clear finished tasks',
        'Journal cleanup could not be confirmed. Use Refresh tasks to check the result before clearing again.'
      );
      this.forgetTasks(removed);
      await this.refreshTasks();
    });
  }

  deleteReviewed(id: string): Promise<void> {
    if (this.deletedTaskIds.has(id)) return Promise.resolve();
    return this.cleanup(async () => {
      try {
        const removed = await this.taskCommand(
          () => this.api.deleteReviewedAgentTask(id),
          'Delete reviewed journal',
          'Journal deletion could not be confirmed. Use Refresh tasks to check the result before deleting again.'
        );
        if (removed > 0) this.forgetTasks([id]);
      } catch (error) {
        if (!isMissingTask(error)) throw error;
        this.forgetTasks([id]);
      }
      await this.refreshTasks();
    });
  }

  private cleanup(action: () => Promise<void>): Promise<void> {
    if (this.cleanupRequest) return this.cleanupRequest;
    const request = this.run(action).finally(() => {
      this.cleanupRequest = null;
    });
    this.cleanupRequest = request;
    return request;
  }

  private forgetTasks(ids: readonly string[]): void {
    if (!ids.length) return;
    for (const id of ids) this.deletedTaskIds.add(id);
    this.tasks.update((tasks) => tasks.filter((task) => !this.deletedTaskIds.has(task.id)));
    const selected = this.selectedId();
    if (selected !== null && this.deletedTaskIds.has(selected)) {
      this.nextSelectionRequest();
      this.selectedId.set(null);
      this.selected.set(null);
    }
  }

  /** One in-flight decision per UUID; no decision is retried automatically. */
  resolve(id: string, approved: boolean): Promise<IpcError | null> {
    if (this.destroyed) return Promise.resolve(new IpcError('cancelled', 'The permission request is no longer available.'));
    const pending = this.approvalDecisions.get(id);
    if (pending) return pending.promise;
    const approval = this.approvals().find((entry) => entry.id === id);
    if (!approval) return Promise.resolve(new IpcError('notFound', 'This permission request is no longer pending.'));
    if (approval.expiresAtMs <= Date.now())
      return Promise.resolve(new IpcError('unavailable', 'This permission request has expired.'));

    let interrupt!: (error: IpcError) => void;
    const interrupted = new Promise<IpcError>((resolve) => {
      interrupt = resolve;
    });
    let complete!: (error: IpcError | null) => void;
    const promise = new Promise<IpcError | null>((resolve) => {
      complete = resolve;
    });
    const operation: ApprovalDecision = { interrupt, promise };
    this.approvalDecisions.set(id, operation);
    this.decidingApprovalIds.set([...this.approvalDecisions.keys()]);
    void this.decideApproval(id, approved, operation, interrupted).then(complete);
    return promise;
  }

  private async decideApproval(
    id: string,
    approved: boolean,
    operation: ApprovalDecision,
    interrupted: Promise<IpcError>
  ): Promise<IpcError | null> {
    const current = () => this.approvalDecisions.get(id) === operation;
    try {
      const result = await withRequestDeadline(
        () => Promise.race([interrupted, this.api.resolveApproval(id, approved)]),
        APPROVAL_DECISION_TIMEOUT_MS,
        'Permission decision',
        this.destroyRef
      );
      if (result instanceof IpcError) return this.uncertainApprovalDecision(result, approved);
      if (!current()) return this.uncertainApprovalDecision(await interrupted, approved);
      // Both boolean responses mean this UUID is no longer awaiting a decision;
      // neither response says that the tool action has completed.
      this.approvals.update((list) => list.filter((a) => a.id !== id));
      return null;
    } catch (error) {
      if (!current()) return this.uncertainApprovalDecision(await interrupted, approved);
      const failure = IpcError.from(error);
      if (
        failure.origin === 'notDispatched' ||
        (failure.origin === 'core' && failure.code !== 'io' && failure.code !== 'internal')
      ) {
        return failure;
      }
      return this.uncertainApprovalDecision(failure, approved);
    } finally {
      if (current()) {
        this.approvalDecisions.delete(id);
        this.decidingApprovalIds.set([...this.approvalDecisions.keys()]);
      }
    }
  }

  private uncertainApprovalDecision(failure: IpcError, approved: boolean): IpcError {
    return new IpcError(
      failure.code,
      `The decision could not be confirmed. ${approved ? 'The action may already be running. ' : ''}Check the task and current permission request before deciding again. ${failure.message}`,
      failure.origin
    );
  }

  private withdrawApprovals(error: IpcError): void {
    const decisions = [...this.approvalDecisions.values()];
    this.approvalDecisions.clear();
    this.approvals.set([]);
    this.decidingApprovalIds.set([]);
    for (const decision of decisions) decision.interrupt(error);
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
  private merge(task: AgentTask, liveEvent = true): void {
    if (this.deletedTaskIds.has(task.id)) return;
    if (liveEvent) {
      for (const updates of this.taskReads) updates.set(task.id, task);
    }
    for (const updates of this.taskListReads) updates.set(task.id, task);
    const summary = summarize(task);
    this.tasks.update((list) => {
      const rest = list.filter((t) => t.id !== task.id);
      return [summary, ...rest].sort((a, b) => b.updatedAtMs - a.updatedAtMs);
    });
    if (liveEvent && this.selectedId() === task.id) {
      this.selected.set(task);
    }
  }

  private publishTaskList(tasks: readonly TaskSummary[], updates: ReadonlyMap<string, AgentTask>): void {
    const merged = new Map(tasks.map((task) => [task.id, task]));
    for (const task of updates.values()) merged.set(task.id, summarize(task));
    this.tasks.set(
      [...merged.values()].filter((task) => !this.deletedTaskIds.has(task.id)).sort((a, b) => b.updatedAtMs - a.updatedAtMs)
    );
  }

  private async readTaskJournal(id: string): Promise<AgentTask | null> {
    try {
      return await this.read(() => this.api.getAgentTask(id), 'Agent task');
    } catch (error) {
      if (isMissingTask(error)) return null;
      throw error;
    }
  }

  private publishTaskJournal(id: string, task: AgentTask | null, updates: ReadonlyMap<string, AgentTask>): void {
    // A capped list cannot establish deletion. An exact core not-found
    // response also takes precedence over queued events for the deleted ID.
    if (task === null) this.forgetTasks([id]);
    else this.selected.set(updates.get(id) ?? task);
  }

  private async observeTasks<T>(action: (updates: Map<string, AgentTask>) => Promise<T>, list = false): Promise<T> {
    const updates = new Map<string, AgentTask>();
    this.taskReads.add(updates);
    if (list) this.taskListReads.add(updates);
    try {
      return await action(updates);
    } finally {
      this.taskReads.delete(updates);
      if (list) this.taskListReads.delete(updates);
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

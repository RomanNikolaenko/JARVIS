import { Injectable, computed, inject, signal } from '@angular/core';
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
import { SettingsStore } from './settings.store';

export const AUDIT_PAGE = 100;
export const MAX_GOAL_LENGTH = 4000;

const FINISHED: readonly TaskStatus[] = ['completed', 'failed', 'cancelled'];

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
  private readonly settingsStore = inject(SettingsStore);
  private started = false;
  private pending = 0;

  readonly status = signal<AgentStatus | null>(null);
  readonly tools = signal<readonly ToolInfo[]>([]);
  readonly tasks = signal<readonly TaskSummary[]>([]);
  readonly selectedId = signal<string | null>(null);
  readonly selected = signal<AgentTask | null>(null);
  readonly approvals = signal<readonly PendingApproval[]>([]);
  readonly audit = signal<readonly AuditEntry[]>([]);
  readonly auditComplete = signal(false);
  readonly verification = signal<AuditVerification | null>(null);
  readonly error = signal<IpcError | null>(null);
  readonly busy = signal(false);

  readonly running = computed(() => this.tasks().filter((t) => !isFinished(t.status)).length);
  readonly hasFinished = computed(() => this.tasks().some((t) => isFinished(t.status)));

  /**
   * Subscribes to task updates and approval requests (app-wide, so an
   * approval request reaches the user on every screen).
   */
  async start(): Promise<void> {
    if (this.started) {
      return;
    }
    this.started = true;
    try {
      await this.api.onAgentTask((task) => this.merge(task));
      await this.api.onApprovals((approvals) => this.approvals.set(approvals));
      this.approvals.set(await this.api.listPendingApprovals());
    } catch (error) {
      this.started = false;
      this.error.set(IpcError.from(error));
    }
  }

  async loadAll(): Promise<void> {
    await Promise.all([this.refreshStatus(), this.loadTools(), this.loadTasks()]);
  }

  refreshStatus(): Promise<void> {
    return this.run(async () => this.status.set(await this.api.getAgentStatus()));
  }

  loadTools(): Promise<void> {
    return this.run(async () => this.tools.set(await this.api.listTools()));
  }

  loadTasks(): Promise<void> {
    return this.run(async () => {
      const tasks = await this.api.listAgentTasks(100);
      this.tasks.set(tasks);
      const selected = this.selectedId();
      if (selected === null && tasks.length) {
        await this.select(tasks[0].id);
      }
    });
  }

  select(id: string | null): Promise<void> {
    this.selectedId.set(id);
    if (id === null) {
      this.selected.set(null);
      return Promise.resolve();
    }
    return this.run(async () => {
      const task = await this.api.getAgentTask(id);
      if (this.selectedId() === id) {
        this.selected.set(task);
      }
    });
  }

  async startTask(request: AgentStartRequest): Promise<AgentTask | null> {
    let task: AgentTask | null = null;
    await this.run(async () => {
      task = await this.api.startAgentTask(request);
      this.merge(task);
      this.selectedId.set(task.id);
      this.selected.set(task);
    });
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
        this.selectedId.set(null);
        this.selected.set(null);
      }
      await this.loadTasks();
    });
  }

  /** The user's decision. A stale request (already decided) is ignored. */
  resolve(id: string, approved: boolean): Promise<void> {
    return this.run(async () => {
      await this.api.resolveApproval(id, approved);
      this.approvals.update((list) => list.filter((a) => a.id !== id));
    });
  }

  loadAudit(more = false): Promise<void> {
    return this.run(async () => {
      const current = more ? this.audit() : [];
      const before = more && current.length ? current[current.length - 1].occurredAtMs : null;
      const page = await this.api.getAuditLog(AUDIT_PAGE, before);
      this.audit.set([...current, ...page]);
      this.auditComplete.set(page.length < AUDIT_PAGE);
    });
  }

  verifyAudit(): Promise<void> {
    return this.run(async () => this.verification.set(await this.api.verifyAuditLog()));
  }

  /** Opens the folder dialog in the core and adds the chosen folder. */
  addWorkspaceFolder(): Promise<void> {
    return this.run(async () => {
      const settings = await this.api.addWorkspaceFolder();
      if (settings) {
        this.settingsStore.settings.set(settings);
        await this.refreshStatus();
      }
    });
  }

  /** Applies a pushed task to the list and the open task. */
  private merge(task: AgentTask): void {
    const summary = summarize(task);
    this.tasks.update((list) => {
      const rest = list.filter((t) => t.id !== task.id);
      return [summary, ...rest].sort((a, b) => b.updatedAtMs - a.updatedAtMs);
    });
    if (this.selectedId() === task.id) {
      this.selected.set(task);
    }
  }

  private async run(action: () => Promise<void>): Promise<void> {
    this.pending++;
    this.busy.set(true);
    try {
      await action();
      this.error.set(null);
    } catch (error) {
      this.error.set(IpcError.from(error));
    } finally {
      this.pending--;
      this.busy.set(this.pending > 0);
    }
  }
}

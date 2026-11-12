import { ChangeDetectionStrategy, Component, DestroyRef, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { RouterLink } from '@angular/router';
import type { AgentTask, StepKind, StepOutcome, TaskStatus, TaskStep } from '@jarvis/shared-types';

import { AgentsStore, MAX_GOAL_LENGTH, hasUnresolvedAction, isFinished } from '../../../core/state/agents.store';
import { ModelsStore, optionKey } from '../../../core/state/models.store';
import { SettingsStore } from '../../../core/state/settings.store';
import { formatShortDate } from '../../../shared/format';
import { Icon, type IconName } from '../../../shared/icon/icon';
import { MarkdownPipe } from '../../../shared/markdown/markdown';
import { StatusBadge } from '../../../shared/status-badge/status-badge';

const STATUS: Record<TaskStatus, { label: string; tone: string }> = {
  pending: { label: 'Pending', tone: 'info' },
  running: { label: 'Running', tone: 'info' },
  waitingForApproval: { label: 'Needs approval', tone: 'warning' },
  paused: { label: 'Paused', tone: 'unknown' },
  completed: { label: 'Completed', tone: 'healthy' },
  unverified: { label: 'Answer unverified', tone: 'warning' },
  failed: { label: 'Failed', tone: 'error' },
  cancelled: { label: 'Cancelled', tone: 'unknown' },
};

const STEP_ICON: Record<StepKind, IconName> = {
  plan: 'events',
  action: 'spark',
  answer: 'chat',
  verification: 'check',
  note: 'clock',
};

const OUTCOME_CHIP: Record<StepOutcome, string> = {
  inProgress: 'warning',
  unknown: 'warning',
  unverified: 'warning',
  succeeded: 'success',
  failed: 'danger',
  denied: 'warning',
  info: '',
};

type ResumeBlockReason = 'disabled' | 'unknownResult' | 'missingIdentity' | 'modelUnavailable';

/** New task form, the task list and the journal of the selected task. */
@Component({
  selector: 'jv-tasks-panel',
  imports: [FormsModule, RouterLink, Icon, StatusBadge, MarkdownPipe],
  templateUrl: './tasks-panel.html',
  styleUrls: ['../agents-shared.scss', './tasks-panel.scss'],
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class TasksPanel {
  protected readonly store = inject(AgentsStore);
  private readonly models = inject(ModelsStore);
  private readonly settings = inject(SettingsStore);
  private readonly destroyRef = inject(DestroyRef);
  private draftRevision = 0;

  protected readonly maxGoalLength = MAX_GOAL_LENGTH;
  protected readonly formatShortDate = formatShortDate;
  protected readonly goal = signal('');
  protected readonly modelKey = signal('');
  protected readonly maxSteps = signal<number | null>(null);
  protected readonly maxMinutes = signal<number | null>(null);
  private readonly deleteConfirmation = signal<{ task: AgentTask; selectionVersion: number } | null>(null);
  protected readonly starting = signal(false);

  protected readonly modelOptions = this.models.chatOptions;
  protected readonly selectedOption = computed(() =>
    this.modelOptions().find((option) => optionKey(option.provider.id, option.model.id) === this.modelKey())
  );
  protected readonly selectionUnavailable = computed(() => this.modelKey() !== '' && !this.selectedOption());
  protected readonly defaults = computed(() => this.settings.settings()?.agents ?? null);
  protected readonly agentsEnabled = computed(
    () => this.settings.settings()?.agents.enabled ?? this.store.status()?.enabled ?? true
  );
  protected readonly modelsLoading = this.models.loading;
  protected readonly canStart = computed(
    () =>
      this.goal().trim().length > 0 &&
      this.goal().length <= MAX_GOAL_LENGTH &&
      this.agentsEnabled() &&
      !this.starting() &&
      !this.selectionUnavailable()
  );
  protected readonly task = this.store.selected;
  protected readonly confirmingJournal = computed(() => {
    const confirmation = this.deleteConfirmation();
    if (
      confirmation &&
      confirmation.task === this.task() &&
      confirmation.task.id === this.store.selectedId() &&
      confirmation.selectionVersion === this.store.selectionVersion()
    ) {
      return confirmation.task;
    }
    return null;
  });
  protected readonly finished = computed(() => {
    const task = this.task();
    return task ? isFinished(task.status) : true;
  });
  protected readonly actionCount = computed(
    () => this.task()?.steps.filter((s) => s.kind === 'action' || s.kind === 'answer').length ?? 0
  );

  constructor() {
    void this.models.refresh();
  }

  protected statusLabel(status: TaskStatus): string {
    return STATUS[status].label;
  }

  protected statusTone(status: TaskStatus): string {
    return STATUS[status].tone;
  }

  protected stepIcon(step: TaskStep): IconName {
    return STEP_ICON[step.kind];
  }

  protected stepTitle(step: TaskStep): string {
    switch (step.kind) {
      case 'plan':
        return 'Plan';
      case 'action':
        return step.tool ?? 'Tool';
      case 'answer':
        return 'Answer';
      case 'verification':
        return 'Check';
      case 'note':
        return 'Note';
    }
  }

  protected outcomeChip(outcome: StepOutcome): string {
    return OUTCOME_CHIP[outcome];
  }

  protected outcomeLabel(outcome: StepOutcome): string {
    return outcome === 'inProgress' ? 'In progress' : outcome;
  }

  protected canDeleteReviewed(task: AgentTask): boolean {
    return isFinished(task.status) && hasUnresolvedAction(task);
  }

  protected resumeBlock(task: AgentTask): ResumeBlockReason | null {
    if (!this.agentsEnabled()) return 'disabled';
    if (hasUnresolvedAction(task)) return 'unknownResult';
    if (!task.providerId?.trim() || !task.model?.trim()) return 'missingIdentity';
    return this.modelOptions().some((option) => option.provider.id === task.providerId && option.model.id === task.model)
      ? null
      : 'modelUnavailable';
  }

  protected canResume(task: AgentTask): boolean {
    return task.status === 'paused' && this.store.selectedId() === task.id && !this.store.busy() && !this.resumeBlock(task);
  }

  protected pause(id: string): void {
    const task = this.task();
    if (task?.id === id && (task.status === 'running' || task.status === 'waitingForApproval')) {
      void this.store.pause(id);
    }
  }

  protected cancel(id: string): void {
    const task = this.task();
    if (task?.id === id && !isFinished(task.status)) {
      void this.store.cancel(id);
    }
  }

  protected async resume(expected: AgentTask): Promise<void> {
    const current = this.task();
    if (
      !current ||
      current.id !== expected.id ||
      current.providerId !== expected.providerId ||
      current.model !== expected.model ||
      expected.status !== 'paused' ||
      !this.canResume(current)
    )
      return;
    await this.store.resume(current.id);
  }

  protected refreshTasks(): void {
    if (!this.store.busy()) void this.store.refreshTasks();
  }

  protected clearFinished(): void {
    if (!this.destroyRef.destroyed && !this.store.busy() && this.store.hasFinished()) void this.store.clearFinished();
  }

  protected refreshModels(): void {
    if (!this.modelsLoading()) void this.models.refresh(true);
  }

  protected selectTask(id: string): void {
    this.cancelDelete();
    void this.store.select(id);
  }

  protected confirmDelete(task: AgentTask, selectionVersion: number): void {
    if (this.canConfirmDelete(task, selectionVersion)) this.deleteConfirmation.set({ task, selectionVersion });
  }

  protected cancelDelete(): void {
    this.deleteConfirmation.set(null);
  }

  protected async deleteReviewed(task: AgentTask, selectionVersion: number): Promise<void> {
    if (this.confirmingJournal() !== task || !this.canConfirmDelete(task, selectionVersion)) return;
    this.cancelDelete();
    await this.store.deleteReviewed(task.id);
  }

  private canConfirmDelete(task: AgentTask, selectionVersion: number): boolean {
    return (
      !this.destroyRef.destroyed &&
      this.task() === task &&
      this.store.selectedId() === task.id &&
      this.store.selectionVersion() === selectionVersion &&
      this.canDeleteReviewed(task) &&
      !this.store.busy()
    );
  }

  protected optionKey(providerId: string, modelId: string): string {
    return optionKey(providerId, modelId);
  }

  protected onGoalChange(value: string): void {
    ++this.draftRevision;
    this.goal.set(value);
  }

  protected onModelChange(value: string): void {
    ++this.draftRevision;
    this.modelKey.set(value);
  }

  protected onStepsChange(value: number | null): void {
    ++this.draftRevision;
    this.maxSteps.set(value);
  }

  protected onMinutesChange(value: number | null): void {
    ++this.draftRevision;
    this.maxMinutes.set(value);
  }

  protected async start(): Promise<void> {
    if (this.destroyRef.destroyed || !this.canStart() || this.store.busy()) {
      return;
    }
    const option = this.selectedOption();
    const minutes = this.maxMinutes();
    const request = {
      goal: this.goal().trim(),
      providerId: option?.provider.id ?? null,
      model: option?.model.id ?? null,
      maxSteps: this.maxSteps(),
      maxDurationSecs: minutes ? Math.round(minutes * 60) : null,
    };
    const revision = this.draftRevision;
    const current = () => !this.destroyRef.destroyed && revision === this.draftRevision;
    this.starting.set(true);
    try {
      const task = await this.store.startTask(request, current);
      if (task && current()) {
        ++this.draftRevision;
        this.goal.set('');
      }
    } finally {
      this.starting.set(false);
    }
  }
}

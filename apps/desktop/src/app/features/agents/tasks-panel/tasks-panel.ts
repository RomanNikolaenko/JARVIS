import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
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

  protected readonly maxGoalLength = MAX_GOAL_LENGTH;
  protected readonly formatShortDate = formatShortDate;
  protected readonly goal = signal('');
  protected readonly modelKey = signal('');
  protected readonly maxSteps = signal<number | null>(null);
  protected readonly maxMinutes = signal<number | null>(null);
  protected readonly confirmDeleteId = signal<string | null>(null);

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
      !this.selectionUnavailable()
  );
  protected readonly task = this.store.selected;
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

  protected refreshModels(): void {
    if (!this.modelsLoading()) void this.models.refresh(true);
  }

  protected async deleteReviewed(id: string): Promise<void> {
    if (this.confirmDeleteId() !== id || this.store.busy()) {
      return;
    }
    this.confirmDeleteId.set(null);
    await this.store.deleteReviewed(id);
  }

  protected optionKey(providerId: string, modelId: string): string {
    return optionKey(providerId, modelId);
  }

  protected async start(): Promise<void> {
    if (!this.canStart() || this.store.busy()) {
      return;
    }
    const option = this.selectedOption();
    const minutes = this.maxMinutes();
    const task = await this.store.startTask({
      goal: this.goal().trim(),
      providerId: option?.provider.id ?? null,
      model: option?.model.id ?? null,
      maxSteps: this.maxSteps(),
      maxDurationSecs: minutes ? Math.round(minutes * 60) : null,
    });
    if (task) {
      this.goal.set('');
    }
  }
}

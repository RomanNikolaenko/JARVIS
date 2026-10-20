import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { RouterLink } from '@angular/router';
import type { StepKind, StepOutcome, TaskStatus, TaskStep } from '@jarvis/shared-types';

import { AgentsStore, MAX_GOAL_LENGTH, isFinished } from '../../../core/state/agents.store';
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
  succeeded: 'success',
  failed: 'danger',
  denied: 'warning',
  info: '',
};

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

  protected readonly modelOptions = this.models.chatOptions;
  protected readonly defaults = computed(() => this.settings.settings()?.agents ?? null);
  protected readonly canStart = computed(
    () =>
      this.goal().trim().length > 0 &&
      this.goal().length <= MAX_GOAL_LENGTH &&
      (this.store.status()?.enabled ?? true),
  );
  protected readonly task = this.store.selected;
  protected readonly finished = computed(() => {
    const task = this.task();
    return task ? isFinished(task.status) : true;
  });
  protected readonly actionCount = computed(
    () => this.task()?.steps.filter((s) => s.kind === 'action' || s.kind === 'answer').length ?? 0,
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

  protected optionKey(providerId: string, modelId: string): string {
    return optionKey(providerId, modelId);
  }

  protected async start(): Promise<void> {
    if (!this.canStart()) {
      return;
    }
    const option = this.modelOptions().find(
      (o) => optionKey(o.provider.id, o.model.id) === this.modelKey(),
    );
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

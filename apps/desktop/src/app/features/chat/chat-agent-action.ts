import { ChangeDetectionStrategy, Component, DestroyRef, computed, inject, input, output, signal } from '@angular/core';
import { Router, RouterLink } from '@angular/router';
import type { ModelRef } from '@jarvis/shared-types';

import { IpcError } from '../../core/ipc/ipc-error';
import { AgentsStore, MAX_GOAL_LENGTH } from '../../core/state/agents.store';
import { Icon } from '../../shared/icon/icon';

export interface ChatAgentSubmission {
  readonly text: string;
  readonly context: number;
  readonly revision: number;
  readonly model: ModelRef | null;
}

/** Explicitly starts a saved agent task from the composer, without chat history. */
@Component({
  selector: 'jv-chat-agent-action',
  imports: [Icon, RouterLink],
  template: `
    <button type="button" class="ghost" [disabled]="!canStart()" (click)="start()">
      <jv-icon name="agent" [size]="16" />
      {{ starting() ? 'Starting agent…' : 'Run as agent' }}
    </button>
    <span class="hint"
      >Uses only this message and the selected model. Agent tasks are saved separately, including from temporary chats.</span
    >
    @if (text().trim().length > maxLength) {
      <span class="error" role="status">Agent goals can contain at most {{ maxLength }} characters.</span>
    }
    @if (error(); as message) {
      <span class="error" role="alert">{{ message }}</span>
    }
    @if (createdId(); as id) {
      <a routerLink="/agents" [queryParams]="{ task: id }">Open created task</a>
    }
  `,
  styles: `
    :host {
      display: flex;
      flex-wrap: wrap;
      align-items: center;
      gap: 8px;
    }
    .hint {
      flex: 1;
      min-width: 200px;
      color: var(--jv-text-dim);
      font-size: 12px;
    }
    .error {
      color: var(--jv-danger);
      font-size: 12px;
    }
    a {
      color: var(--jv-accent);
    }
  `,
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class ChatAgentAction {
  readonly text = input.required<string>();
  readonly model = input<ModelRef | null>(null);
  readonly disabled = input(false);
  /** Live getters also observe changes that have not rendered yet. */
  readonly readContext = input<() => number>(() => 0);
  readonly readRevision = input<() => number>(() => 0);
  readonly isCurrent = input<(submission: ChatAgentSubmission) => boolean>(() => true);
  readonly started = output<ChatAgentSubmission>();
  readonly busyChange = output<boolean>();
  private readonly agents = inject(AgentsStore);
  private readonly router = inject(Router);
  private readonly destroyRef = inject(DestroyRef);
  protected readonly maxLength = MAX_GOAL_LENGTH;
  protected readonly starting = signal(false);
  protected readonly error = signal<string | null>(null);
  protected readonly createdId = signal<string | null>(null);
  protected readonly canStart = computed(() => {
    const goal = this.text().trim();
    return !this.disabled() && !this.starting() && goal.length > 0 && goal.length <= MAX_GOAL_LENGTH;
  });

  protected async start(): Promise<void> {
    if (this.destroyRef.destroyed || !this.canStart()) return;
    const model = this.model();
    const submission: ChatAgentSubmission = Object.freeze({
      text: this.text(),
      context: this.readContext()(),
      revision: this.readRevision()(),
      model: model ? Object.freeze({ providerId: model.providerId, model: model.model }) : null,
    });
    const sameModel = () =>
      submission.model?.providerId === this.model()?.providerId && submission.model?.model === this.model()?.model;
    const current = () =>
      !this.destroyRef.destroyed &&
      submission.context === this.readContext()() &&
      submission.revision === this.readRevision()() &&
      submission.text === this.text() &&
      sameModel() &&
      this.isCurrent()(submission);
    if (!current()) return;
    this.starting.set(true);
    this.busyChange.emit(true);
    this.error.set(null);
    this.createdId.set(null);
    let navigationRevision: number | null = null;
    try {
      const task = await this.agents.startTask(
        {
          goal: submission.text.trim(),
          providerId: submission.model?.providerId ?? null,
          model: submission.model?.model ?? null,
          maxSteps: null,
          maxDurationSecs: null,
        },
        current
      );
      if (!current()) return;
      if (!task) {
        this.error.set(this.agents.error()?.message ?? 'Could not start the agent task.');
        return;
      }
      this.createdId.set(task.id);
      this.started.emit(submission);
      if (this.destroyRef.destroyed || submission.context !== this.readContext()()) return;
      // The parent may consume the submitted draft synchronously. A failed
      // navigation still belongs to that consumed revision, not a newer edit.
      navigationRevision = this.readRevision()();
      await this.router.navigate(['/agents'], { queryParams: { task: task.id } });
    } catch (error) {
      if (
        navigationRevision === null
          ? current()
          : !this.destroyRef.destroyed &&
            submission.context === this.readContext()() &&
            navigationRevision === this.readRevision()() &&
            sameModel()
      )
        this.error.set(IpcError.from(error).message);
    } finally {
      if (!this.destroyRef.destroyed) {
        this.starting.set(false);
        this.busyChange.emit(false);
      }
    }
  }
}

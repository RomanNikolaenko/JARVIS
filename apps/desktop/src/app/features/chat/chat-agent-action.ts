import { ChangeDetectionStrategy, Component, computed, inject, input, output, signal } from '@angular/core';
import { Router, RouterLink } from '@angular/router';
import type { ModelRef } from '@jarvis/shared-types';

import { IpcError } from '../../core/ipc/ipc-error';
import { AgentsStore, MAX_GOAL_LENGTH } from '../../core/state/agents.store';
import { Icon } from '../../shared/icon/icon';

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
  readonly started = output<string>();
  readonly busyChange = output<boolean>();
  private readonly agents = inject(AgentsStore);
  private readonly router = inject(Router);
  protected readonly maxLength = MAX_GOAL_LENGTH;
  protected readonly starting = signal(false);
  protected readonly error = signal<string | null>(null);
  protected readonly createdId = signal<string | null>(null);
  protected readonly canStart = computed(() => {
    const goal = this.text().trim();
    return !this.disabled() && !this.starting() && goal.length > 0 && goal.length <= MAX_GOAL_LENGTH;
  });

  protected async start(): Promise<void> {
    if (!this.canStart()) return;
    const text = this.text();
    const model = this.model();
    this.starting.set(true);
    this.busyChange.emit(true);
    this.error.set(null);
    this.createdId.set(null);
    try {
      const task = await this.agents.startTask({
        goal: text.trim(),
        providerId: model?.providerId ?? null,
        model: model?.model ?? null,
        maxSteps: null,
        maxDurationSecs: null,
      });
      if (!task) {
        this.error.set(this.agents.error()?.message ?? 'Could not start the agent task.');
        return;
      }
      this.createdId.set(task.id);
      this.started.emit(text);
      await this.router.navigate(['/agents'], { queryParams: { task: task.id } });
    } catch (error) {
      this.error.set(IpcError.from(error).message);
    } finally {
      this.starting.set(false);
      this.busyChange.emit(false);
    }
  }
}

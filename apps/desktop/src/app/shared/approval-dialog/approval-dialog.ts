import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  computed,
  effect,
  inject,
  signal,
  viewChild,
} from '@angular/core';
import { RouterLink } from '@angular/router';

import { AgentsStore } from '../../core/state/agents.store';
import { Icon } from '../icon/icon';

const RISK_LABEL = { low: 'Low risk', medium: 'Medium risk', high: 'High risk' } as const;

/**
 * Asks the user about an agent action the security policy holds. Shown on
 * every screen; the oldest request comes first. "Deny" has the focus, so an
 * accidental Enter never approves.
 */
@Component({
  selector: 'jv-approval-dialog',
  imports: [Icon, RouterLink],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (current(); as approval) {
      <div class="backdrop">
        <section
          class="dialog"
          [class]="'risk-' + approval.risk"
          role="alertdialog"
          aria-modal="true"
          aria-labelledby="approval-title"
          aria-describedby="approval-summary"
        >
          <header>
            <jv-icon name="shield" [size]="20" />
            <h2 id="approval-title">An agent asks for permission</h2>
            <span class="risk">{{ riskLabel(approval.risk) }}</span>
          </header>
          <p id="approval-summary" class="summary">{{ approval.summary }}</p>
          <dl>
            <div>
              <dt>Tool</dt>
              <dd class="mono">{{ approval.tool }}</dd>
            </div>
            <div>
              <dt>Why it asks</dt>
              <dd>{{ approval.reason }}</dd>
            </div>
            <div>
              <dt>Time left</dt>
              <dd>{{ remaining() }}</dd>
            </div>
          </dl>
          <details>
            <summary>Exact input</summary>
            <pre>{{ approval.details }}</pre>
          </details>
          @if (approval.risk === 'high') {
            <p class="warning">
              <jv-icon name="warning" [size]="14" />
              This action can change or delete data or run programs. Allow it only if you expected
              it.
            </p>
          }
          <footer>
            @if (count() > 1) {
              <span class="queue">1 of {{ count() }} waiting</span>
            }
            @if (approval.taskId) {
              <a class="task-link" routerLink="/agents">Open the task</a>
            }
            <button #deny type="button" class="ghost" (click)="decide(approval.id, false)">
              Deny
            </button>
            <button type="button" class="primary" (click)="decide(approval.id, true)">
              Allow once
            </button>
          </footer>
        </section>
      </div>
    }
  `,
  styles: `
    .backdrop {
      position: fixed;
      inset: 0;
      z-index: 50;
      display: grid;
      place-items: center;
      padding: 24px;
      background: rgba(5, 8, 12, 0.72);
    }
    .dialog {
      width: min(560px, 100%);
      max-height: calc(100vh - 48px);
      overflow: auto;
      padding: 18px 20px;
      border: 1px solid var(--jv-border-strong);
      border-radius: var(--jv-radius);
      background: var(--jv-surface);
      box-shadow: 0 20px 60px rgba(0, 0, 0, 0.5);
    }
    .dialog.risk-high {
      border-color: rgba(248, 113, 113, 0.6);
    }
    .dialog.risk-medium {
      border-color: rgba(251, 191, 36, 0.5);
    }
    header {
      display: flex;
      align-items: center;
      gap: 10px;
      color: var(--jv-accent);
    }
    h2 {
      flex: 1;
      margin: 0;
      font-size: 15px;
      font-weight: 600;
      color: var(--jv-text);
    }
    .risk {
      padding: 2px 8px;
      border: 1px solid currentColor;
      border-radius: 999px;
      font: 600 10.5px/1.6 var(--jv-mono);
      text-transform: uppercase;
      letter-spacing: 0.06em;
    }
    .risk-low .risk {
      color: var(--jv-success);
    }
    .risk-medium .risk {
      color: var(--jv-warning);
    }
    .risk-high .risk {
      color: var(--jv-danger);
    }
    .summary {
      margin: 14px 0 10px;
      font-size: 14px;
      line-height: 1.5;
      overflow-wrap: anywhere;
    }
    dl {
      display: grid;
      gap: 6px;
      margin: 0 0 10px;
    }
    dl div {
      display: grid;
      grid-template-columns: 110px minmax(0, 1fr);
      gap: 8px;
    }
    dt {
      font-size: 12px;
      color: var(--jv-text-dim);
    }
    dd {
      margin: 0;
      font-size: 12.5px;
      overflow-wrap: anywhere;
    }
    .mono {
      font-family: var(--jv-mono);
    }
    details {
      margin-bottom: 10px;
      font-size: 12.5px;
      color: var(--jv-text-muted);
    }
    summary {
      cursor: pointer;
    }
    pre {
      max-height: 220px;
      overflow: auto;
      margin: 8px 0 0;
      padding: 10px;
      border-radius: var(--jv-radius-sm);
      background: var(--jv-bg);
      font: 12px/1.45 var(--jv-mono);
      white-space: pre-wrap;
      overflow-wrap: anywhere;
    }
    .warning {
      display: flex;
      gap: 6px;
      margin: 0 0 10px;
      font-size: 12.5px;
      color: var(--jv-danger);
    }
    footer {
      display: flex;
      align-items: center;
      justify-content: flex-end;
      gap: 10px;
      margin-top: 14px;
    }
    .queue,
    .task-link {
      margin-right: auto;
      font-size: 12px;
      color: var(--jv-text-dim);
    }
    .task-link {
      color: var(--jv-accent);
    }
    .queue + .task-link {
      margin-right: 0;
    }
  `,
})
export class ApprovalDialog {
  private readonly store = inject(AgentsStore);
  private readonly now = signal(Date.now());
  private readonly denyButton = viewChild<ElementRef<HTMLButtonElement>>('deny');

  protected readonly current = computed(() => this.store.approvals()[0] ?? null);
  protected readonly count = computed(() => this.store.approvals().length);
  protected readonly remaining = computed(() => {
    const approval = this.current();
    if (!approval) {
      return '';
    }
    const seconds = Math.max(0, Math.round((approval.expiresAtMs - this.now()) / 1000));
    const minutes = Math.floor(seconds / 60);
    return minutes > 0 ? `${minutes} min ${seconds % 60} s` : `${seconds} s`;
  });

  constructor() {
    const timer = setInterval(() => this.now.set(Date.now()), 1000);
    inject(DestroyRef).onDestroy(() => clearInterval(timer));
    // Focus "Deny" whenever a new request is shown.
    effect(() => {
      const id = this.current()?.id;
      const button = this.denyButton();
      if (id && button) {
        button.nativeElement.focus();
      }
    });
  }

  protected riskLabel(risk: keyof typeof RISK_LABEL): string {
    return RISK_LABEL[risk];
  }

  protected decide(id: string, approved: boolean): void {
    void this.store.resolve(id, approved);
  }
}

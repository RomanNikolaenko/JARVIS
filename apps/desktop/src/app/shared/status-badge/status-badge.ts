import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';

export type BadgeTone = 'ok' | 'warn' | 'bad' | 'muted' | 'info';

const TONES: Record<string, BadgeTone> = {
  healthy: 'ok',
  degraded: 'warn',
  unhealthy: 'bad',
  offline: 'bad',
  unknown: 'muted',
  info: 'info',
  debug: 'muted',
  warning: 'warn',
  error: 'bad',
};

/** Small status pill. Maps health statuses and event levels to colors. */
@Component({
  selector: 'jv-status-badge',
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { '[class]': '"tone-" + tone()' },
  template: `<span class="dot"></span>{{ label() ?? status() }}`,
  styles: `
    :host {
      display: inline-flex;
      align-items: center;
      gap: 6px;
      padding: 2px 8px;
      border-radius: 999px;
      border: 1px solid currentColor;
      font: 600 10.5px/1.6 var(--jv-mono);
      letter-spacing: 0.08em;
      text-transform: uppercase;
      white-space: nowrap;
    }
    .dot {
      width: 6px;
      height: 6px;
      border-radius: 50%;
      background: currentColor;
      box-shadow: 0 0 8px currentColor;
    }
    :host(.tone-ok) {
      color: var(--jv-success);
    }
    :host(.tone-warn) {
      color: var(--jv-warning);
    }
    :host(.tone-bad) {
      color: var(--jv-danger);
    }
    :host(.tone-info) {
      color: var(--jv-accent);
    }
    :host(.tone-muted) {
      color: var(--jv-text-dim);
    }
  `,
})
export class StatusBadge {
  readonly status = input.required<string>();
  readonly label = input<string>();
  protected readonly tone = computed<BadgeTone>(() => TONES[this.status()] ?? 'muted');
}

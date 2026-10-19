import { ChangeDetectionStrategy, Component, input } from '@angular/core';

/** Framed HUD panel with an optional title and header actions. */
@Component({
  selector: 'jv-hud-panel',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (heading()) {
      <header class="panel-header">
        <h2 class="panel-title">{{ heading() }}</h2>
        <ng-content select="[panelActions]" />
      </header>
    }
    <div class="panel-body"><ng-content /></div>
  `,
  styles: `
    :host {
      position: relative;
      display: flex;
      flex-direction: column;
      min-width: 0;
      background: linear-gradient(180deg, var(--jv-surface) 0%, var(--jv-bg-elevated) 100%);
      border: 1px solid var(--jv-border);
      border-radius: var(--jv-radius);
      padding: 16px 18px;
    }
    /* HUD corner brackets */
    :host::before,
    :host::after {
      content: '';
      position: absolute;
      width: 12px;
      height: 12px;
      border-color: var(--jv-accent);
      border-style: solid;
      opacity: 0.55;
      pointer-events: none;
    }
    :host::before {
      top: -1px;
      left: -1px;
      border-width: 1px 0 0 1px;
      border-top-left-radius: var(--jv-radius);
    }
    :host::after {
      bottom: -1px;
      right: -1px;
      border-width: 0 1px 1px 0;
      border-bottom-right-radius: var(--jv-radius);
    }
    .panel-header {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 12px;
      margin-bottom: 12px;
    }
    .panel-title {
      margin: 0;
      font: 600 11px/1.2 var(--jv-mono);
      letter-spacing: 0.14em;
      text-transform: uppercase;
      color: var(--jv-text-muted);
    }
    .panel-body {
      flex: 1;
      min-height: 0;
    }
  `,
})
export class HudPanel {
  readonly heading = input<string>();
}

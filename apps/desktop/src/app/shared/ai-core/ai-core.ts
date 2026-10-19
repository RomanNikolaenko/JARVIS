import { ChangeDetectionStrategy, Component, input } from '@angular/core';

/** Visual state of the AI core. */
export type CoreState = 'idle' | 'thinking' | 'speaking' | 'degraded' | 'offline';

/**
 * Animated JARVIS core. Animation is pure CSS (transform/opacity only, GPU
 * friendly) and is disabled for reduced motion.
 */
@Component({
  selector: 'jv-ai-core',
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: {
    '[class]': '"state-" + state()',
    '[style.--core-size]': 'size() + "px"',
    role: 'img',
    '[attr.aria-label]': '"JARVIS core: " + state()',
  },
  template: `
    <svg viewBox="0 0 200 200" class="core-svg">
      <defs>
        <radialGradient id="jv-core-glow" cx="50%" cy="50%" r="50%">
          <stop offset="0%" stop-color="var(--core-hot)" />
          <stop offset="40%" stop-color="var(--core-color)" stop-opacity="0.8" />
          <stop offset="100%" stop-color="var(--core-color)" stop-opacity="0" />
        </radialGradient>
      </defs>
      <g class="ring ring-outer">
        <circle cx="100" cy="100" r="92" stroke-dasharray="120 24 40 24" />
      </g>
      <g class="ring ring-mid">
        <circle cx="100" cy="100" r="74" stroke-dasharray="6 8" />
      </g>
      <g class="ring ring-inner">
        <circle cx="100" cy="100" r="56" stroke-dasharray="70 18" />
      </g>
      <circle class="halo" cx="100" cy="100" r="46" fill="url(#jv-core-glow)" />
      <circle class="nucleus" cx="100" cy="100" r="16" />
    </svg>
  `,
  styles: `
    :host {
      --core-color: var(--jv-accent);
      --core-hot: #e0fbff;
      --spin: 28s;
      --pulse: 4s;
      display: inline-block;
      width: var(--core-size);
      height: var(--core-size);
    }
    :host(.state-thinking) {
      --core-color: var(--jv-accent-2);
      --spin: 6s;
      --pulse: 1.2s;
    }
    :host(.state-speaking) {
      --core-color: var(--jv-accent-2);
      --spin: 12s;
      --pulse: 0.7s;
    }
    :host(.state-degraded) {
      --core-color: var(--jv-warning);
      --core-hot: #fff4d6;
    }
    :host(.state-offline) {
      --core-color: var(--jv-text-dim);
      --core-hot: #9fb0c0;
    }
    .core-svg {
      width: 100%;
      height: 100%;
      overflow: visible;
    }
    .ring {
      transform-origin: 100px 100px;
    }
    .ring circle {
      fill: none;
      stroke: var(--core-color);
    }
    .ring-outer circle {
      stroke-width: 2;
      opacity: 0.75;
    }
    .ring-mid circle {
      stroke-width: 1.2;
      opacity: 0.45;
    }
    .ring-inner circle {
      stroke-width: 2.5;
      opacity: 0.85;
    }
    .ring-outer {
      animation: spin var(--spin) linear infinite;
    }
    .ring-mid {
      animation: spin calc(var(--spin) * 1.6) linear infinite reverse;
    }
    .ring-inner {
      animation: spin calc(var(--spin) * 0.7) linear infinite;
    }
    .halo {
      transform-origin: 100px 100px;
      animation: pulse var(--pulse) ease-in-out infinite;
    }
    .nucleus {
      fill: var(--core-hot);
      filter: drop-shadow(0 0 8px var(--core-color));
    }
    :host(.state-offline) .ring,
    :host(.state-offline) .halo {
      animation: none;
    }
    :host-context(.jv-reduced-motion) .ring,
    :host-context(.jv-reduced-motion) .halo {
      animation: none;
    }
    @media (prefers-reduced-motion: reduce) {
      .ring,
      .halo {
        animation: none;
      }
    }
    @keyframes spin {
      to {
        transform: rotate(360deg);
      }
    }
    @keyframes pulse {
      0%,
      100% {
        transform: scale(0.92);
        opacity: 0.75;
      }
      50% {
        transform: scale(1.06);
        opacity: 1;
      }
    }
  `,
})
export class AiCore {
  readonly state = input<CoreState>('idle');
  readonly size = input(220);
}

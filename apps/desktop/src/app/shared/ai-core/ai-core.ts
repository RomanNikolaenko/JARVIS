import { ChangeDetectionStrategy, Component, input } from '@angular/core';

/** Visual state of the AI core. */
export type CoreState = 'idle' | 'thinking' | 'speaking' | 'degraded' | 'offline';

/**
 * Animated JARVIS core, disabled for reduced motion.
 *
 * Every moving part is an HTML element animated with `transform` and
 * `opacity` only, so the browser runs the animation on the compositor.
 * Rotating SVG elements (even an outer `<svg>`) recalculates styles on the
 * main thread on every frame instead: measured at about half a CPU core
 * while the dashboard was idle.
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
    <div class="ring ring-outer">
      <svg viewBox="0 0 200 200" aria-hidden="true">
        <circle cx="100" cy="100" r="92" stroke-dasharray="120 24 40 24" />
      </svg>
    </div>
    <div class="ring ring-mid">
      <svg viewBox="0 0 200 200" aria-hidden="true">
        <circle cx="100" cy="100" r="74" stroke-dasharray="6 8" />
      </svg>
    </div>
    <div class="ring ring-inner">
      <svg viewBox="0 0 200 200" aria-hidden="true">
        <circle cx="100" cy="100" r="56" stroke-dasharray="70 18" />
      </svg>
    </div>
    <div class="halo"></div>
    <div class="nucleus"></div>
  `,
  styles: `
    :host {
      --core-color: var(--jv-accent);
      --core-hot: #e0fbff;
      --spin: 28s;
      --pulse: 4s;
      position: relative;
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
    .ring,
    .halo,
    .nucleus {
      position: absolute;
      border-radius: 50%;
    }
    .ring {
      inset: 0;
      will-change: transform;
    }
    .ring svg {
      display: block;
      width: 100%;
      height: 100%;
      overflow: visible;
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
    /* r = 46 of 200 */
    .halo {
      inset: 27%;
      background: radial-gradient(
        circle closest-side,
        var(--core-hot) 0%,
        color-mix(in srgb, var(--core-color) 80%, transparent) 40%,
        transparent 100%
      );
      will-change: transform, opacity;
      animation: pulse var(--pulse) ease-in-out infinite;
    }
    /* r = 16 of 200 */
    .nucleus {
      inset: 42%;
      background: var(--core-hot);
      box-shadow: 0 0 8px var(--core-color);
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

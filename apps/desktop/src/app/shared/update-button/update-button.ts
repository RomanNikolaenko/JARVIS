import { ChangeDetectionStrategy, Component, computed, inject } from '@angular/core';

import { UpdatesStore } from '../../core/state/updates.store';
import { Icon, type IconName } from '../icon/icon';

/** Release notes can be long; the tooltip shows the start. */
const MAX_NOTES_CHARS = 300;

/**
 * The update button in the top bar. It shows the running version, turns
 * into "Update to X" when a newer version is found (pulsing until the user
 * has looked at it), and a click downloads, verifies and installs it; JARVIS
 * restarts. Without an update a click checks again.
 */
@Component({
  selector: 'jv-update-button',
  imports: [Icon],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (updates.status()) {
      <button
        type="button"
        class="update"
        [class.available]="offering()"
        [class.failed]="failed()"
        [class.attention]="updates.attention()"
        [disabled]="updates.busy()"
        [attr.title]="title()"
        [attr.aria-label]="title()"
        (pointerenter)="updates.markSeen()"
        (focus)="updates.markSeen()"
        (click)="activate()">
        <jv-icon [name]="icon()" [size]="14" />
        <span class="label">{{ label() }}</span>
        @if (updates.attention()) {
          <span class="dot" aria-hidden="true"></span>
        }
      </button>
    }
  `,
  styles: `
    .update {
      position: relative;
      display: inline-flex;
      align-items: center;
      gap: 6px;
      padding: 2px 10px;
      border: 1px solid var(--jv-border);
      border-radius: 999px;
      background: transparent;
      color: var(--jv-text-dim);
      font: 500 12px/1.6 var(--jv-mono);
      white-space: nowrap;
      cursor: pointer;
    }
    .update:hover:not(:disabled) {
      color: var(--jv-text);
      border-color: var(--jv-text-dim);
    }
    .update:disabled {
      cursor: default;
    }
    .update.available {
      color: var(--jv-accent);
      border-color: var(--jv-accent);
    }
    .update.available:hover:not(:disabled) {
      color: var(--jv-accent);
      border-color: var(--jv-accent);
      background: color-mix(in srgb, var(--jv-accent) 12%, transparent);
    }
    .update.failed {
      color: var(--jv-warning);
      border-color: var(--jv-warning);
    }
    .dot {
      position: absolute;
      top: -3px;
      right: -3px;
      width: 8px;
      height: 8px;
      border-radius: 50%;
      background: var(--jv-accent);
    }
    // The ring only scales and fades (compositor work): the idle renderer
    // stays free, and it pauses in the background like every animation.
    .dot::after {
      content: '';
      position: absolute;
      inset: 0;
      border-radius: 50%;
      background: var(--jv-accent);
      animation: update-pulse 2.4s ease-out infinite;
    }
    @keyframes update-pulse {
      0% {
        transform: scale(1);
        opacity: 0.6;
      }
      70%,
      100% {
        transform: scale(2.8);
        opacity: 0;
      }
    }
  `,
})
export class UpdateButton {
  protected readonly updates = inject(UpdatesStore);

  /** An update can be installed (or installed again after a failure). */
  protected readonly offering = computed(() => this.updates.available() !== null || this.updates.retryVersion() !== null);
  /** The last check failed and there is nothing to install. */
  protected readonly failed = computed(() => this.updates.status()?.phase === 'failed' && !this.offering());

  protected readonly icon = computed<IconName>(() => {
    const phase = this.updates.status()?.phase;
    if (this.offering() || phase === 'downloading' || phase === 'installing') {
      return 'download';
    }
    return phase === 'upToDate' ? 'check' : 'refresh';
  });

  protected readonly label = computed(() => {
    const status = this.updates.status();
    const version = this.updates.current() ?? '';
    switch (status?.phase) {
      case 'checking':
        return 'Checking…';
      case 'downloading': {
        const percent = this.updates.progressPercent();
        return percent === null ? 'Updating…' : `Updating… ${percent}%`;
      }
      case 'installing':
        return 'Restarting…';
      default:
        break;
    }
    const available = this.updates.available();
    if (available) {
      return `Update to ${available.version}`;
    }
    const retry = this.updates.retryVersion();
    return retry ? `Retry update ${retry}` : `v${version}`;
  });

  protected readonly title = computed(() => {
    const status = this.updates.status();
    const reported = this.updates.error()?.message ?? status?.error ?? null;
    // Core errors may end without a full stop; the tooltip adds what to do next.
    const error = reported && !/[.!?]$/.test(reported.trim()) ? `${reported.trim()}.` : reported;
    const available = this.updates.available();
    if (status?.phase === 'downloading' || status?.phase === 'installing') {
      return 'Downloading and installing the update; JARVIS restarts when it is done';
    }
    if (available) {
      const notes = available.notes?.trim();
      const shortNotes = notes && notes.length > MAX_NOTES_CHARS ? `${notes.slice(0, MAX_NOTES_CHARS)}…` : notes;
      return (
        `Download, verify and install JARVIS ${available.version}. Running agent tasks, plugins and local models ` +
        `stop, then JARVIS restarts.${shortNotes ? `\n\n${shortNotes}` : ''}`
      );
    }
    const retry = this.updates.retryVersion();
    if (retry) {
      return `${error ?? 'The update failed.'} Click to try ${retry} again.`;
    }
    if (status?.phase === 'checking') {
      return 'Checking for updates…';
    }
    if (status?.phase === 'failed') {
      return `${error ?? 'The update check failed.'} Click to check again.`;
    }
    if (status?.phase === 'upToDate') {
      return `JARVIS ${status.currentVersion} is the latest version. Click to check again.`;
    }
    return 'Check for updates';
  });

  protected activate(): void {
    if (this.offering()) {
      void this.updates.update();
    } else {
      void this.updates.check();
    }
  }
}

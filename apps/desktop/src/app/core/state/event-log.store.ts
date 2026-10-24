import { DestroyRef, Injectable, inject, signal } from '@angular/core';
import type { SystemEvent } from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';
import { withRequestDeadline } from '../ipc/request-deadline';

export const MAX_UI_EVENTS = 500;
export const EVENT_LOG_TIMEOUT_MS = 30_000;

/** Live system event log: initial history plus pushed events, deduplicated by id. */
@Injectable({ providedIn: 'root' })
export class EventLogStore {
  private readonly api = inject(JarvisApi);
  private readonly destroyRef = inject(DestroyRef);
  private started = false;
  private destroyed = false;
  private starting: Promise<void> | null = null;
  private disconnect: (() => void) | null = null;

  readonly events = signal<readonly SystemEvent[]>([]);
  readonly error = signal<IpcError | null>(null);
  readonly connecting = signal(false);

  constructor() {
    inject(DestroyRef).onDestroy(() => {
      this.destroyed = true;
      this.disconnect?.();
    });
  }

  /** Subscribes first, then loads history, so no event falls in between. */
  start(): Promise<void> {
    if (this.destroyed || this.started) return Promise.resolve();
    if (this.starting) return this.starting;
    this.connecting.set(true);
    const attempt = this.subscribe().finally(() => {
      if (this.starting === attempt) {
        this.starting = null;
        this.connecting.set(false);
      }
    });
    this.starting = attempt;
    return attempt;
  }

  connectionLost(error: IpcError): void {
    if (this.destroyed) return;
    this.disconnect?.();
    this.disconnect = null;
    this.started = false;
    this.starting = null;
    this.connecting.set(false);
    this.error.set(error);
  }

  private async subscribe(): Promise<void> {
    let active = true;
    let unlisten: (() => void) | null = null;
    let loading = true;
    let live: readonly SystemEvent[] = [];
    const dispose = () => {
      active = false;
      unlisten?.();
      unlisten = null;
      live = [];
    };
    this.disconnect = dispose;
    try {
      await withRequestDeadline(
        () =>
          this.api
            .onSystemEvent((event) => {
              if (!active) return;
              if (loading) live = mergeEvents(live, [event]);
              this.events.update((current) => mergeEvents(current, [event]));
            })
            .then((cleanup) => {
              // This continuation also runs after timeout/destruction, when the
              // native listener must be released instead of retained.
              if (!active) cleanup();
              else unlisten = cleanup;
            }),
        EVENT_LOG_TIMEOUT_MS,
        'System event subscription',
        this.destroyRef
      );
      if (!active) return;
      const history = await withRequestDeadline(
        () => this.api.getRecentEvents(MAX_UI_EVENTS),
        EVENT_LOG_TIMEOUT_MS,
        'System event history',
        this.destroyRef
      );
      if (!active) return;
      // The core's ring buffer is authoritative: event IDs restart with the core.
      this.events.set(mergeEvents(history, live));
      loading = false;
      live = [];
      this.started = true;
      this.error.set(null);
    } catch (error) {
      if (active) this.error.set(IpcError.from(error));
      dispose();
    }
  }
}

function mergeEvents(current: readonly SystemEvent[], incoming: readonly SystemEvent[]): readonly SystemEvent[] {
  const byId = new Map<number, SystemEvent>();
  for (const event of current) {
    byId.set(event.id, event);
  }
  for (const event of incoming) {
    byId.set(event.id, event);
  }
  return [...byId.values()].sort((a, b) => a.id - b.id).slice(-MAX_UI_EVENTS);
}

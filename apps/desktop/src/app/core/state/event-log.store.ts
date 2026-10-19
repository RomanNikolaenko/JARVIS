import { Injectable, inject, signal } from '@angular/core';
import type { SystemEvent } from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';

export const MAX_UI_EVENTS = 500;

/** Live system event log: initial history plus pushed events, deduplicated by id. */
@Injectable({ providedIn: 'root' })
export class EventLogStore {
  private readonly api = inject(JarvisApi);
  private started = false;

  readonly events = signal<readonly SystemEvent[]>([]);
  readonly error = signal<IpcError | null>(null);

  /** Subscribes first, then loads history, so no event falls in between. */
  async start(): Promise<void> {
    if (this.started) {
      return;
    }
    this.started = true;
    try {
      await this.api.onSystemEvent((event) => this.merge([event]));
      this.merge(await this.api.getRecentEvents(MAX_UI_EVENTS));
      this.error.set(null);
    } catch (error) {
      this.started = false;
      this.error.set(IpcError.from(error));
    }
  }

  private merge(incoming: readonly SystemEvent[]): void {
    this.events.update((current) => {
      const byId = new Map<number, SystemEvent>();
      for (const event of current) {
        byId.set(event.id, event);
      }
      for (const event of incoming) {
        byId.set(event.id, event);
      }
      return [...byId.values()].sort((a, b) => a.id - b.id).slice(-MAX_UI_EVENTS);
    });
  }
}

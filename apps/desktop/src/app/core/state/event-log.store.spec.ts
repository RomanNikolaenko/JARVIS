import { TestBed } from '@angular/core/testing';
import type { SystemEvent } from '@jarvis/shared-types';

import { JarvisApi } from '../ipc/jarvis-api';
import { EventLogStore } from './event-log.store';

function event(id: number, message = `event ${id}`): SystemEvent {
  return { id, timestampMs: 1_000 + id, level: 'info', category: 'lifecycle', message };
}

describe('EventLogStore', () => {
  it('merges history and live events without duplicates, ordered by id', async () => {
    let push: (e: SystemEvent) => void = () => undefined;
    const api = {
      onSystemEvent: vi.fn(async (handler: (e: SystemEvent) => void) => {
        push = handler;
        // An event arrives between subscribing and loading history.
        handler(event(3));
        return () => undefined;
      }),
      getRecentEvents: vi.fn(async () => [event(1), event(2), event(3)]),
    };
    TestBed.configureTestingModule({ providers: [{ provide: JarvisApi, useValue: api }] });
    const store = TestBed.inject(EventLogStore);

    await store.start();
    push(event(4));

    expect(store.events().map((e) => e.id)).toEqual([1, 2, 3, 4]);
    expect(store.error()).toBeNull();
  });

  it('exposes subscription errors and allows retrying', async () => {
    const api = {
      onSystemEvent: vi.fn().mockRejectedValueOnce({ code: 'unavailable', message: 'offline' }),
      getRecentEvents: vi.fn(async () => []),
    };
    TestBed.configureTestingModule({ providers: [{ provide: JarvisApi, useValue: api }] });
    const store = TestBed.inject(EventLogStore);

    await store.start();
    expect(store.error()?.code).toBe('unavailable');

    api.onSystemEvent.mockResolvedValueOnce(() => undefined);
    await store.start();
    expect(store.error()).toBeNull();
  });
});

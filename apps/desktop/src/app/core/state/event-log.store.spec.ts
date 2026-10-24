import { TestBed } from '@angular/core/testing';
import type { SystemEvent } from '@jarvis/shared-types';

import { JarvisApi } from '../ipc/jarvis-api';
import { EVENT_LOG_TIMEOUT_MS, EventLogStore, MAX_UI_EVENTS } from './event-log.store';
import { IpcError } from '../ipc/ipc-error';

function event(id: number, message = `event ${id}`): SystemEvent {
  return { id, timestampMs: 1_000 + id, level: 'info', category: 'lifecycle', message };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, resolve, reject };
}

function setup() {
  let push!: (e: SystemEvent) => void;
  const cleanup = vi.fn(() => undefined);
  const api = {
    onSystemEvent: vi.fn(async (handler: (e: SystemEvent) => void) => {
      push = handler;
      return cleanup;
    }),
    getRecentEvents: vi.fn(async (): Promise<SystemEvent[]> => []),
  };
  TestBed.configureTestingModule({ providers: [{ provide: JarvisApi, useValue: api }] });
  return { api, cleanup, store: TestBed.inject(EventLogStore), push: (e: SystemEvent) => push(e), handler: () => push };
}

describe('EventLogStore', () => {
  it('times out registration, retries and releases the late listener exactly once', async () => {
    const { api, store } = setup();
    const lateCleanup = vi.fn(() => undefined);
    const registration = deferred<typeof lateCleanup>();
    let oldHandler!: (e: SystemEvent) => void;
    api.onSystemEvent.mockImplementationOnce((handler) => {
      oldHandler = handler;
      return registration.promise;
    });
    vi.useFakeTimers();
    try {
      const first = store.start();
      expect(store.start()).toBe(first);
      expect(store.connecting()).toBe(true);
      await vi.advanceTimersByTimeAsync(EVENT_LOG_TIMEOUT_MS);
      await first;
      expect(store.connecting()).toBe(false);
      expect(store.error()?.code).toBe('timeout');
      expect(api.getRecentEvents).not.toHaveBeenCalled();
      api.getRecentEvents.mockResolvedValueOnce([event(1)]);
      await store.start();
      registration.resolve(lateCleanup);
      await vi.advanceTimersByTimeAsync(0);
      oldHandler(event(99));
      expect(store.events().map((e) => e.id)).toEqual([1]);
      expect(store.error()).toBeNull();
      expect(lateCleanup).toHaveBeenCalledTimes(1);
      TestBed.resetTestingModule();
      expect(lateCleanup).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it.each(['resolve', 'reject'])('ignores late history %s after timeout and retry', async (outcome) => {
    const { api, store, cleanup, push } = setup();
    const history = deferred<SystemEvent[]>();
    api.getRecentEvents.mockReturnValueOnce(history.promise);
    vi.useFakeTimers();
    try {
      const first = store.start();
      await vi.advanceTimersByTimeAsync(0);
      push(event(1, 'live before timeout'));
      await vi.advanceTimersByTimeAsync(EVENT_LOG_TIMEOUT_MS);
      await first;
      expect(cleanup).toHaveBeenCalledTimes(1);
      expect(store.events()[0].message).toBe('live before timeout');
      expect(store.error()?.code).toBe('timeout');
      api.getRecentEvents.mockResolvedValueOnce([event(2)]);
      await store.start();
      if (outcome === 'resolve') history.resolve([event(99)]);
      else history.reject(new Error('old history failed'));
      await vi.advanceTimersByTimeAsync(0);
      expect(store.events().map((e) => e.id)).toEqual([2]);
      expect(store.error()).toBeNull();
      expect(store.connecting()).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });

  it.each(['registration', 'history'])(
    'settles pending %s on destruction without waiting for native completion',
    async (stage) => {
      const { api, store, cleanup } = setup();
      const registration = deferred<typeof cleanup>();
      const history = deferred<SystemEvent[]>();
      if (stage === 'registration') api.onSystemEvent.mockReturnValueOnce(registration.promise);
      else api.getRecentEvents.mockReturnValueOnce(history.promise);
      const first = store.start();
      if (stage === 'history') await vi.waitFor(() => expect(api.getRecentEvents).toHaveBeenCalled());
      TestBed.resetTestingModule();
      await first;
      expect(store.connecting()).toBe(false);
      expect(store.error()).toBeNull();
      registration.resolve(cleanup);
      history.resolve([event(99)]);
      await Promise.resolve();
      await store.start();
      expect(cleanup).toHaveBeenCalledTimes(1);
      expect(store.events()).toEqual([]);
      expect(api.onSystemEvent).toHaveBeenCalledTimes(1);
    }
  );

  it('cleans up after a history failure and ignores obsolete callbacks on retry', async () => {
    const { api, cleanup, store, handler, push } = setup();
    api.getRecentEvents.mockRejectedValueOnce(new Error('History unavailable'));
    await store.start();
    const old = handler();
    expect(cleanup).toHaveBeenCalledTimes(1);
    expect(store.error()?.message).toBe('History unavailable');
    await store.start();
    old(event(99));
    push(event(1));
    expect(store.events().map((e) => e.id)).toEqual([1]);
    expect(store.error()).toBeNull();
  });

  it('replaces the old session history and keeps live events during reconnect', async () => {
    const { api, store, push } = setup();
    api.getRecentEvents.mockResolvedValueOnce([event(100, 'old session')]);
    await store.start();
    store.connectionLost(new IpcError('unavailable', 'Offline'));
    const history = deferred<SystemEvent[]>();
    api.getRecentEvents.mockReturnValueOnce(history.promise);
    const connecting = store.start();
    await vi.waitFor(() => expect(api.getRecentEvents).toHaveBeenCalledTimes(2));
    push(event(2, 'live'));
    history.resolve([event(1), event(2, 'snapshot')]);
    await connecting;
    expect(store.events().map((e) => e.id)).toEqual([1, 2]);
    expect(store.events()[1].message).toBe('live');
  });

  it('bounds both history and events received while history is pending', async () => {
    const { api, store, push } = setup();
    const history = deferred<SystemEvent[]>();
    api.getRecentEvents.mockReturnValueOnce(history.promise);
    const connecting = store.start();
    await vi.waitFor(() => expect(api.getRecentEvents).toHaveBeenCalled());
    for (let id = 1; id <= MAX_UI_EVENTS + 10; id++) push(event(id));
    history.resolve([event(0)]);
    await connecting;
    expect(store.events()).toHaveLength(MAX_UI_EVENTS);
    expect(store.events()[0].id).toBe(11);
  });

  it('shares an attempt and releases a listener registered after destruction', async () => {
    const { api, cleanup, store } = setup();
    const registered = deferred<typeof cleanup>();
    api.onSystemEvent.mockReturnValueOnce(registered.promise);
    const first = store.start();
    expect(store.start()).toBe(first);
    TestBed.resetTestingModule();
    registered.resolve(cleanup);
    await first;
    expect(cleanup).toHaveBeenCalledTimes(1);
    expect(api.getRecentEvents).not.toHaveBeenCalled();
  });

  it('allows a new connection while an obsolete history request is pending', async () => {
    const { api, store } = setup();
    const old = deferred<SystemEvent[]>();
    api.getRecentEvents.mockReturnValueOnce(old.promise);
    const first = store.start();
    await vi.waitFor(() => expect(api.getRecentEvents).toHaveBeenCalled());
    store.connectionLost(new IpcError('unavailable', 'Offline'));
    api.getRecentEvents.mockResolvedValueOnce([event(2)]);
    await store.start();
    old.resolve([event(99)]);
    await first;
    expect(store.events().map((e) => e.id)).toEqual([2]);
    expect(store.error()).toBeNull();
  });

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

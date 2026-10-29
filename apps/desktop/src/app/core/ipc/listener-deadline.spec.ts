import { DestroyRef } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import { listenerWithDeadline } from './listener-deadline';

describe('listenerWithDeadline', () => {
  beforeEach(() => {
    TestBed.configureTestingModule({});
  });

  it('releases a registration that arrives after timeout exactly once', async () => {
    vi.useFakeTimers();
    try {
      let resolve!: (stop: () => void) => void;
      const stop = vi.fn();
      const result = listenerWithDeadline(
        new Promise((done) => {
          resolve = done;
        }),
        100,
        'Subscription',
        TestBed.inject(DestroyRef)
      );
      const failure = expect(result).rejects.toMatchObject({ code: 'timeout' });
      await vi.advanceTimersByTimeAsync(101);
      await failure;
      resolve(stop);
      await Promise.resolve();
      expect(stop).toHaveBeenCalledTimes(1);
      TestBed.resetTestingModule();
      expect(stop).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it('settles disposal before a native registration responds', async () => {
    let resolve!: (stop: () => void) => void;
    const stop = vi.fn();
    const result = listenerWithDeadline(
      new Promise((done) => {
        resolve = done;
      }),
      10_000,
      'Subscription',
      TestBed.inject(DestroyRef)
    );
    const failure = expect(result).rejects.toMatchObject({ code: 'cancelled' });
    TestBed.resetTestingModule();
    await failure;
    resolve(stop);
    await Promise.resolve();
    expect(stop).toHaveBeenCalledTimes(1);
  });

  it('returns idempotent cleanup for an active subscription', async () => {
    const stop = vi.fn();
    const dispose = await listenerWithDeadline(Promise.resolve(stop), 100, 'Subscription', TestBed.inject(DestroyRef));
    expect(stop).not.toHaveBeenCalled();
    dispose();
    dispose();
    expect(stop).toHaveBeenCalledTimes(1);
  });
});

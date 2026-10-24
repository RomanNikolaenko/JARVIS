import { DestroyRef, ErrorHandler, inject } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import { pollWhileVisible } from './poll';

describe('pollWhileVisible', () => {
  let hidden: boolean;
  const errors = { handleError: vi.fn() };

  beforeEach(() => {
    vi.useFakeTimers();
    hidden = false;
    vi.spyOn(document, 'hidden', 'get').mockImplementation(() => hidden);
    errors.handleError.mockClear();
    TestBed.configureTestingModule({ providers: [{ provide: ErrorHandler, useValue: errors }] });
  });

  afterEach(() => {
    TestBed.resetTestingModule();
    vi.restoreAllMocks();
    vi.useRealTimers();
  });

  function start(task: () => unknown) {
    TestBed.runInInjectionContext(() => pollWhileVisible(task, 1000, inject(DestroyRef)));
  }

  function visibility(value: boolean) {
    hidden = value;
    document.dispatchEvent(new Event('visibilitychange'));
  }

  it('runs immediately, stops timers while hidden and refreshes on return', async () => {
    const task = vi.fn();
    start(task);
    expect(task).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1000);
    expect(task).toHaveBeenCalledTimes(2);
    visibility(true);
    expect(vi.getTimerCount()).toBe(0);
    await vi.advanceTimersByTimeAsync(5000);
    expect(task).toHaveBeenCalledTimes(2);
    visibility(false);
    expect(task).toHaveBeenCalledTimes(3);
    visibility(false);
    expect(task).toHaveBeenCalledTimes(3);
    expect(vi.getTimerCount()).toBe(1);
    await vi.advanceTimersByTimeAsync(1000);
    expect(task).toHaveBeenCalledTimes(4);
  });

  it('does not poll when initialized in a hidden window', async () => {
    hidden = true;
    const task = vi.fn();
    start(task);
    await vi.advanceTimersByTimeAsync(5000);
    expect(task).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
    visibility(false);
    expect(task).toHaveBeenCalledTimes(1);
  });

  it('does not overlap a pending task across timers or visibility changes', async () => {
    let finish!: () => void;
    const task = vi.fn().mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          finish = resolve;
        })
    );
    start(task);
    await vi.advanceTimersByTimeAsync(3000);
    visibility(true);
    visibility(false);
    expect(task).toHaveBeenCalledTimes(1);
    finish();
    await vi.advanceTimersByTimeAsync(1000);
    expect(task).toHaveBeenCalledTimes(2);
  });

  it.each(['sync', 'async'])('reports a %s failure and continues polling', async (kind) => {
    const error = new Error('Polling failed');
    const task = vi.fn().mockImplementationOnce(() => {
      if (kind === 'sync') throw error;
      return Promise.reject(error);
    });
    start(task);
    await vi.advanceTimersByTimeAsync(0);
    expect(errors.handleError).toHaveBeenCalledWith(error);
    await vi.advanceTimersByTimeAsync(1000);
    expect(task).toHaveBeenCalledTimes(2);
  });

  it('removes the visibility listener and timer on destruction', async () => {
    const task = vi.fn();
    start(task);
    TestBed.resetTestingModule();
    visibility(true);
    visibility(false);
    await vi.advanceTimersByTimeAsync(5000);
    expect(task).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(0);
  });
});

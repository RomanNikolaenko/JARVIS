import { DestroyRef, ErrorHandler, inject } from '@angular/core';

/**
 * Runs `task` immediately and then every `intervalMs` while the window is
 * visible, and immediately after returning to the window. Hidden windows
 * have no polling timer. Calls never overlap; in-flight work is not cancelled.
 * Must be called in an Angular injection context (e.g. a constructor).
 */
export function pollWhileVisible(task: () => unknown, intervalMs: number, destroyRef: DestroyRef): void {
  if (destroyRef.destroyed) return;
  const errors = inject(ErrorHandler);
  let stopped = false;
  let running = false;
  let visible = typeof document === 'undefined' || !document.hidden;
  let handle: ReturnType<typeof setInterval> | undefined;
  const tick = (): void => {
    if (stopped || !visible || running) return;
    running = true;
    void (async () => {
      try {
        await task();
      } catch (error) {
        if (!stopped) errors.handleError(error);
      } finally {
        running = false;
      }
    })();
  };
  const resume = (): void => {
    handle = setInterval(tick, intervalMs);
    tick();
  };
  const onVisibility = (): void => {
    const next = !document.hidden;
    if (next === visible || stopped) return;
    visible = next;
    clearInterval(handle);
    handle = undefined;
    if (visible) resume();
  };
  if (typeof document !== 'undefined') document.addEventListener('visibilitychange', onVisibility);
  destroyRef.onDestroy(() => {
    stopped = true;
    clearInterval(handle);
    if (typeof document !== 'undefined') document.removeEventListener('visibilitychange', onVisibility);
  });
  if (visible) resume();
}

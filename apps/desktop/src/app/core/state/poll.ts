import { DestroyRef } from '@angular/core';

/**
 * Runs `task` immediately and then every `intervalMs` while the window is
 * visible. Stops when `destroyRef` is destroyed. Keeps idle CPU usage low:
 * hidden or minimized windows do not poll.
 */
export function pollWhileVisible(
  task: () => unknown,
  intervalMs: number,
  destroyRef: DestroyRef,
): void {
  const tick = (): void => {
    if (typeof document === 'undefined' || !document.hidden) {
      void task();
    }
  };
  tick();
  const handle = setInterval(tick, intervalMs);
  destroyRef.onDestroy(() => clearInterval(handle));
}

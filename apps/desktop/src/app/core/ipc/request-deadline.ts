import type { DestroyRef } from '@angular/core';

import { IpcError } from './ipc-error';

/** Bounds frontend waiting; does not cancel an already dispatched native command. */
export async function withRequestDeadline<T>(
  request: () => Promise<T>,
  timeoutMs: number,
  label: string,
  destroyRef: DestroyRef
): Promise<T> {
  if (destroyRef.destroyed) throw new IpcError('cancelled', 'Request disposed.');
  let timer!: ReturnType<typeof setTimeout>;
  let unregister!: () => void;
  const deadline = new Promise<never>((_resolve, reject) => {
    timer = setTimeout(
      () => reject(new IpcError('timeout', `${label} did not respond within ${timeoutMs / 1000} seconds.`)),
      timeoutMs
    );
    unregister = destroyRef.onDestroy(() => reject(new IpcError('cancelled', 'Request disposed.')));
  });
  try {
    // The race handles late rejections without allowing late state updates.
    return await Promise.race([request(), deadline]);
  } finally {
    clearTimeout(timer);
    unregister();
  }
}

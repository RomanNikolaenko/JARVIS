import type { DestroyRef } from '@angular/core';

import { withRequestDeadline } from './request-deadline';

/** A timed-out registration still owns cleanup when the native reply arrives. */
export async function listenerWithDeadline(
  registration: Promise<() => void>,
  timeoutMs: number,
  label: string,
  destroyRef: DestroyRef
): Promise<() => void> {
  let active = true;
  let stop: (() => void) | undefined;
  const dispose = () => {
    active = false;
    stop?.();
    stop = undefined;
  };
  // Attach even when already destroyed, so a dispatched registration cannot leak.
  const pending = registration.then((cleanup) => {
    if (!active || destroyRef.destroyed) cleanup();
    else stop = cleanup;
  });
  try {
    await withRequestDeadline(() => pending, timeoutMs, label, destroyRef);
    return dispose;
  } catch (error) {
    dispose();
    // withRequestDeadline may reject before subscribing to pending on disposal.
    void pending.catch(() => undefined);
    throw error;
  }
}

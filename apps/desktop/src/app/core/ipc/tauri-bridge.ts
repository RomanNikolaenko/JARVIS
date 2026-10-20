import { Injectable } from '@angular/core';
import { Channel, invoke, type InvokeArgs } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import { IpcError } from './ipc-error';

/**
 * The only place that touches `@tauri-apps/api`. Everything else goes through
 * {@link JarvisApi}, which keeps the UI testable and the IPC surface explicit.
 */
@Injectable({ providedIn: 'root' })
export class TauriBridge {
  /** Whether the page runs inside the JARVIS desktop shell. */
  get available(): boolean {
    if (typeof window === 'undefined') {
      return false;
    }
    // Checks `invoke` itself: a leftover global without it (e.g. after the
    // Tauri test mocks are cleared) cannot reach the core.
    const internals = (window as { __TAURI_INTERNALS__?: { invoke?: unknown } }).__TAURI_INTERNALS__;
    return typeof internals?.invoke === 'function';
  }

  async invoke<T>(command: string, args?: InvokeArgs): Promise<T> {
    this.ensureAvailable();
    try {
      return await invoke<T>(command, args);
    } catch (error) {
      throw IpcError.from(error);
    }
  }

  async listen<T>(event: string, handler: (payload: T) => void): Promise<UnlistenFn> {
    this.ensureAvailable();
    try {
      return await listen<T>(event, (message) => handler(message.payload));
    } catch (error) {
      throw IpcError.from(error);
    }
  }

  channel<T>(onMessage: (message: T) => void): Channel<T> {
    return new Channel<T>(onMessage);
  }

  private ensureAvailable(): void {
    if (!this.available) {
      throw new IpcError(
        'unavailable',
        'The JARVIS core is not reachable. Start the UI through the desktop app (npm run tauri dev).'
      );
    }
  }
}

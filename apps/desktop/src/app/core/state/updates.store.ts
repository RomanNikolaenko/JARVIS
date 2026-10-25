import { Injectable, computed, inject, signal } from '@angular/core';
import type { UpdateStatus } from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';

/**
 * Application updates (signed, from GitHub Releases). The core pushes the
 * status; nothing is downloaded or installed without the user's click.
 */
@Injectable({ providedIn: 'root' })
export class UpdatesStore {
  private readonly api = inject(JarvisApi);
  private started = false;

  readonly status = signal<UpdateStatus | null>(null);
  readonly error = signal<IpcError | null>(null);

  readonly available = computed(() => {
    const status = this.status();
    return status?.phase === 'available' ? status.available : null;
  });
  /** Downloading or installing: the app is about to restart. */
  readonly busy = computed(() => {
    const phase = this.status()?.phase;
    return phase === 'checking' || phase === 'downloading' || phase === 'installing';
  });
  readonly progressPercent = computed(() => {
    const status = this.status();
    if (!status?.totalBytes || status.downloadedBytes === null) {
      return null;
    }
    return Math.min(100, Math.round((status.downloadedBytes / status.totalBytes) * 100));
  });

  async start(): Promise<void> {
    if (this.started) {
      return;
    }
    this.started = true;
    try {
      await this.api.onUpdateStatus((status) => this.status.set(status));
      this.status.set(await this.api.getUpdateStatus());
    } catch (error) {
      this.started = false;
      this.error.set(IpcError.from(error));
    }
  }

  async check(): Promise<void> {
    try {
      this.status.set(await this.api.checkForUpdate());
      this.error.set(null);
    } catch (error) {
      this.error.set(IpcError.from(error));
    }
  }

  /** Downloads, verifies and installs the version the user confirmed; JARVIS restarts. */
  async install(version: string): Promise<void> {
    try {
      await this.api.installUpdate(version);
      this.error.set(null);
    } catch (error) {
      this.error.set(IpcError.from(error));
    }
  }
}

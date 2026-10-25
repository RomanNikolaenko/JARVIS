import { Injectable, computed, inject, signal } from '@angular/core';
import type { UpdateStatus } from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';

/** The update version the user has already looked at (this computer only). */
const SEEN_KEY = 'jarvis.update-seen';

function readSeen(): string | null {
  try {
    return localStorage.getItem(SEEN_KEY);
  } catch {
    return null;
  }
}

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
  /** The running version, once the core has reported it. */
  readonly current = computed(() => this.status()?.currentVersion ?? null);
  /** Checking, downloading or installing. */
  readonly busy = computed(() => {
    const phase = this.status()?.phase;
    return phase === 'checking' || phase === 'downloading' || phase === 'installing';
  });
  /** Downloading or installing: the app is about to restart. */
  readonly installing = computed(() => {
    const phase = this.status()?.phase;
    return phase === 'downloading' || phase === 'installing';
  });
  /** A version whose download or installation failed; trying again checks first. */
  readonly retryVersion = computed(() => {
    const status = this.status();
    return status?.phase === 'failed' ? (status.available?.version ?? null) : null;
  });

  private readonly seenVersion = signal<string | null>(readSeen());
  /** A new version nobody has looked at yet: the update button draws attention. */
  readonly attention = computed(() => {
    const available = this.available();
    return available !== null && this.seenVersion() !== available.version;
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

  /**
   * The update button: installs the version found. After a failed attempt
   * the core no longer holds that download, so it checks again first.
   */
  async update(): Promise<void> {
    if (this.busy()) {
      return;
    }
    if (!this.available() && this.retryVersion()) {
      await this.check();
    }
    const available = this.available();
    if (available) {
      this.markSeen();
      await this.install(available.version);
    }
  }

  /** The user has looked at the found version: the button stops drawing attention. */
  markSeen(): void {
    const available = this.available();
    if (!available || this.seenVersion() === available.version) {
      return;
    }
    this.seenVersion.set(available.version);
    try {
      localStorage.setItem(SEEN_KEY, available.version);
    } catch {
      // Storage may be unavailable; the button then pulses again after a restart.
    }
  }
}

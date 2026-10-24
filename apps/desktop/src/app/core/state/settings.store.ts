import { DestroyRef, Injectable, computed, inject, signal } from '@angular/core';
import type { Settings } from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';

@Injectable({ providedIn: 'root' })
export class SettingsStore {
  private readonly api = inject(JarvisApi);
  private readonly destroyRef = inject(DestroyRef);
  private revision = 0;

  readonly settings = signal<Settings | null>(null);
  readonly error = signal<IpcError | null>(null);
  readonly saving = signal(false);

  readonly reducedMotion = computed(() => this.settings()?.interface.reducedMotion ?? false);

  async load(): Promise<void> {
    if (this.saving() || this.destroyRef.destroyed) return;
    const revision = ++this.revision;
    try {
      const settings = await this.api.getSettings();
      if (revision !== this.revision || this.destroyRef.destroyed) return;
      this.settings.set(settings);
      this.error.set(null);
    } catch (error) {
      if (revision === this.revision && !this.destroyRef.destroyed) this.error.set(IpcError.from(error));
    }
  }

  /** Persists settings through the core, which validates them. */
  save(next: Settings): Promise<boolean> {
    return this.write(() => this.api.updateSettings(next));
  }

  /** The native folder picker persists settings too, so it shares the write guard. */
  addWorkspaceFolder(): Promise<boolean> {
    return this.write(() => this.api.addWorkspaceFolder());
  }

  private async write(action: () => Promise<Settings | null>): Promise<boolean> {
    // Reject overlapping snapshots rather than report another caller's save as success.
    if (this.saving() || this.destroyRef.destroyed) return false;
    this.revision++;
    this.saving.set(true);
    this.error.set(null);
    try {
      const settings = await action();
      if (this.destroyRef.destroyed || settings === null) return false;
      this.settings.set(settings);
      this.error.set(null);
      return true;
    } catch (error) {
      if (!this.destroyRef.destroyed) this.error.set(IpcError.from(error));
      return false;
    } finally {
      this.saving.set(false);
    }
  }
}

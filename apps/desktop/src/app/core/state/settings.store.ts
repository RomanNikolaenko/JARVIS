import { Injectable, computed, inject, signal } from '@angular/core';
import type { Settings } from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';

@Injectable({ providedIn: 'root' })
export class SettingsStore {
  private readonly api = inject(JarvisApi);

  readonly settings = signal<Settings | null>(null);
  readonly error = signal<IpcError | null>(null);
  readonly saving = signal(false);

  readonly reducedMotion = computed(() => this.settings()?.interface.reducedMotion ?? false);

  async load(): Promise<void> {
    try {
      this.settings.set(await this.api.getSettings());
      this.error.set(null);
    } catch (error) {
      this.error.set(IpcError.from(error));
    }
  }

  /** Persists settings through the core, which validates them. */
  async save(next: Settings): Promise<boolean> {
    this.saving.set(true);
    try {
      this.settings.set(await this.api.updateSettings(next));
      this.error.set(null);
      return true;
    } catch (error) {
      this.error.set(IpcError.from(error));
      return false;
    } finally {
      this.saving.set(false);
    }
  }
}

import { Injectable, computed, inject, signal } from '@angular/core';
import type { CaptureInfo, VisionStatus, WindowInfo } from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';

/** Images in one message (the core enforces the same limit). */
export const MAX_ATTACHMENTS = 4;

/**
 * Captures attached to the next chat message. Every capture starts from a
 * button the user pressed; the preview is shown before anything is sent.
 */
@Injectable({ providedIn: 'root' })
export class VisionStore {
  private readonly api = inject(JarvisApi);

  readonly status = signal<VisionStatus | null>(null);
  readonly windows = signal<readonly WindowInfo[]>([]);
  readonly attachments = signal<readonly CaptureInfo[]>([]);
  readonly busy = signal(false);
  readonly error = signal<IpcError | null>(null);

  readonly capabilities = computed(() => this.status()?.capabilities ?? null);
  readonly full = computed(() => this.attachments().length >= MAX_ATTACHMENTS);

  async refresh(): Promise<void> {
    await this.run(async () => this.status.set(await this.api.getVisionStatus()));
  }

  async loadWindows(): Promise<void> {
    await this.run(async () => this.windows.set(await this.api.listWindows()));
  }

  captureScreen(display: string | null, hideJarvis = true): Promise<void> {
    return this.add(() => this.api.captureScreen(display, hideJarvis));
  }

  captureWindow(id: string): Promise<void> {
    return this.add(() => this.api.captureWindow(id));
  }

  captureSelection(): Promise<void> {
    return this.add(() => this.api.captureSelection());
  }

  attachFile(): Promise<void> {
    return this.add(() => this.api.attachImageFile());
  }

  attachPhoto(data: string): Promise<void> {
    return this.add(() => this.api.attachPhoto(data));
  }

  async remove(id: string): Promise<void> {
    this.attachments.update((list) => list.filter((a) => a.id !== id));
    try {
      await this.api.discardCapture(id);
    } catch (error) {
      console.error('Failed to discard a capture', error);
    }
  }

  /** Hands the attachments to a message (the core uses each one once). */
  take(): string[] {
    const ids = this.attachments().map((a) => a.id);
    this.attachments.set([]);
    return ids;
  }

  private async add(capture: () => Promise<CaptureInfo | null>): Promise<void> {
    if (this.full()) {
      this.error.set(new IpcError('limitExceeded', `A message may have at most ${MAX_ATTACHMENTS} images`));
      return;
    }
    await this.run(async () => {
      const info = await capture();
      if (info) {
        this.attachments.update((list) => [...list, info]);
      }
    });
  }

  private async run(action: () => Promise<void>): Promise<void> {
    this.busy.set(true);
    try {
      await action();
      this.error.set(null);
    } catch (error) {
      const failure = IpcError.from(error);
      // Closing the system dialog is not an error.
      this.error.set(failure.code === 'cancelled' ? null : failure);
    } finally {
      this.busy.set(false);
    }
  }
}

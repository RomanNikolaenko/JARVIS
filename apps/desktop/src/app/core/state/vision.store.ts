import { DestroyRef, Injectable, computed, inject, signal } from '@angular/core';
import type { CaptureInfo, VisionStatus, WindowInfo } from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';
import { withRequestDeadline } from '../ipc/request-deadline';

/** Images in one message (the core enforces the same limit). */
export const MAX_ATTACHMENTS = 4;

/**
 * Captures attached to the next chat message. Every capture starts from a
 * button the user pressed; the preview is shown before anything is sent.
 */
@Injectable({ providedIn: 'root' })
export class VisionStore {
  private readonly api = inject(JarvisApi);
  private readonly destroyRef = inject(DestroyRef);
  private attachmentEpoch = 0;
  private capturing = false;
  private pending = 0;
  private statusEpoch = 0;
  private windowsEpoch = 0;

  constructor() {
    this.destroyRef.onDestroy(() => this.clear());
  }

  /** A different conversation must never inherit the previous draft's captures. */
  clear(): void {
    ++this.attachmentEpoch;
    const discarded = this.attachments();
    this.attachments.set([]);
    this.error.set(null);
    for (const capture of discarded) void this.discard(capture.id);
  }

  private async discard(id: string): Promise<void> {
    try {
      await this.api.discardCapture(id);
    } catch (error) {
      console.error('Failed to discard a capture', error);
    }
  }

  readonly status = signal<VisionStatus | null>(null);
  readonly windows = signal<readonly WindowInfo[]>([]);
  readonly attachments = signal<readonly CaptureInfo[]>([]);
  readonly busy = signal(false);
  readonly error = signal<IpcError | null>(null);

  readonly capabilities = computed(() => this.status()?.capabilities ?? null);
  readonly full = computed(() => this.attachments().length >= MAX_ATTACHMENTS);

  async refresh(): Promise<void> {
    const epoch = ++this.statusEpoch;
    const current = () => epoch === this.statusEpoch && !this.destroyRef.destroyed;
    await this.run(async () => {
      const status = await withRequestDeadline(() => this.api.getVisionStatus(), 10_000, 'Vision status', this.destroyRef);
      if (current()) this.status.set(status);
    }, current);
  }

  async loadWindows(): Promise<void> {
    const epoch = ++this.windowsEpoch;
    const current = () => epoch === this.windowsEpoch && !this.destroyRef.destroyed;
    await this.run(async () => {
      const windows = await withRequestDeadline(() => this.api.listWindows(), 10_000, 'Window list', this.destroyRef);
      if (current()) this.windows.set(windows);
    }, current);
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
  take(): CaptureInfo[] {
    ++this.attachmentEpoch;
    const taken = [...this.attachments()];
    this.attachments.set([]);
    return taken;
  }

  private async add(capture: () => Promise<CaptureInfo | null>): Promise<void> {
    if (this.destroyRef.destroyed || this.capturing) return;
    if (this.full()) {
      this.error.set(new IpcError('limitExceeded', `A message may have at most ${MAX_ATTACHMENTS} images`));
      return;
    }
    const epoch = this.attachmentEpoch;
    this.capturing = true;
    try {
      await this.run(
        async () => {
          const info = await capture();
          if (!info) return;
          if (epoch !== this.attachmentEpoch || this.destroyRef.destroyed) await this.discard(info.id);
          else this.attachments.update((list) => [...list, info]);
        },
        () => epoch === this.attachmentEpoch
      );
    } finally {
      this.capturing = false;
    }
  }

  private async run(action: () => Promise<void>, current: () => boolean = () => true): Promise<void> {
    if (this.destroyRef.destroyed) return;
    if (this.pending === 0) this.error.set(null);
    ++this.pending;
    this.busy.set(true);
    try {
      await action();
    } catch (error) {
      const failure = IpcError.from(error);
      // Closing the system dialog is not an error.
      if (current() && !this.destroyRef.destroyed && failure.code !== 'cancelled') this.error.set(failure);
    } finally {
      this.busy.set(--this.pending > 0);
    }
  }
}

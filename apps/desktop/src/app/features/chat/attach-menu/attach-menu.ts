import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';

import { VisionStore } from '../../../core/state/vision.store';
import { CameraDialog } from '../../../shared/camera-dialog/camera-dialog';
import { Icon } from '../../../shared/icon/icon';

/** Whether this WebView can use a camera at all. */
export function cameraAvailable(): boolean {
  return typeof navigator !== 'undefined' && typeof navigator.mediaDevices?.getUserMedia === 'function';
}

/**
 * The "attach an image" button of the chat: screen, window, an area picked
 * in the system dialog, a file or a camera photo.
 */
@Component({
  selector: 'jv-attach-menu',
  imports: [CameraDialog, Icon],
  templateUrl: './attach-menu.html',
  styleUrl: './attach-menu.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { '(document:keydown.escape)': 'close()' },
})
export class AttachMenu {
  protected readonly vision = inject(VisionStore);
  protected readonly open = signal(false);
  protected readonly choosingWindow = signal(false);
  protected readonly cameraOpen = signal(false);
  protected readonly hideJarvis = signal(true);
  protected readonly cameraSupported = cameraAvailable();
  protected readonly capabilities = this.vision.capabilities;
  protected readonly displays = computed(() => this.vision.status()?.displays ?? []);

  protected toggle(): void {
    if (this.open()) {
      this.close();
      return;
    }
    this.open.set(true);
    this.choosingWindow.set(false);
    void this.vision.refresh();
  }

  protected close(): void {
    this.open.set(false);
    this.choosingWindow.set(false);
  }

  protected async screen(display: string | null): Promise<void> {
    this.close();
    await this.vision.captureScreen(display, this.hideJarvis());
  }

  protected async showWindows(): Promise<void> {
    this.choosingWindow.set(true);
    await this.vision.loadWindows();
  }

  protected async window(id: string): Promise<void> {
    this.close();
    await this.vision.captureWindow(id);
  }

  protected async selection(): Promise<void> {
    this.close();
    await this.vision.captureSelection();
  }

  protected async file(): Promise<void> {
    this.close();
    await this.vision.attachFile();
  }

  protected camera(): void {
    this.close();
    this.cameraOpen.set(true);
  }

  protected onHideJarvis(event: Event): void {
    this.hideJarvis.set((event.target as HTMLInputElement).checked);
  }
}

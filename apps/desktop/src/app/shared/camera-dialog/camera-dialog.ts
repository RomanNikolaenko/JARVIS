import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  type ElementRef,
  afterNextRender,
  inject,
  output,
  signal,
  viewChild,
} from '@angular/core';

import { VisionStore } from '../../core/state/vision.store';
import { Icon } from '../icon/icon';

/** JPEG quality of camera photos. */
const PHOTO_QUALITY = 0.9;

/**
 * Takes one camera photo. The camera runs only while this dialog is open
 * (the browser and the system ask for permission and show their own
 * indicators); it is released when the photo is taken or the dialog closes.
 */
@Component({
  selector: 'jv-camera-dialog',
  imports: [Icon],
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { '(document:keydown.escape)': 'cancel()' },
  template: `
    <div class="backdrop"></div>
    <div class="dialog" role="dialog" aria-modal="true" aria-label="Camera photo">
      <header>
        <span class="live" [class.on]="streaming()"
          ><span class="dot"></span>{{ streaming() ? 'Camera on' : 'Starting the camera…' }}</span
        >
      </header>
      <video #video autoplay playsinline muted [class.hidden]="!streaming()"></video>
      @if (error(); as message) {
        <p class="error" role="alert">{{ message }}</p>
      }
      <footer>
        <button type="button" class="ghost" (click)="cancel()">Cancel</button>
        <button type="button" class="primary" (click)="take()" [disabled]="!streaming() || saving()">
          <jv-icon name="camera" [size]="16" /> Take photo
        </button>
      </footer>
    </div>
  `,
  styles: `
    .backdrop {
      position: fixed;
      inset: 0;
      z-index: 50;
      background: rgb(0 0 0 / 55%);
    }
    .dialog {
      position: fixed;
      top: 50%;
      left: 50%;
      z-index: 51;
      display: flex;
      flex-direction: column;
      gap: 10px;
      width: min(640px, calc(100vw - 32px));
      padding: 14px;
      border: 1px solid var(--jv-border-strong);
      border-radius: var(--jv-radius);
      background: var(--jv-bg-elevated);
      transform: translate(-50%, -50%);
    }
    video {
      width: 100%;
      border-radius: var(--jv-radius-sm);
      background: #000;
    }
    .hidden {
      display: none;
    }
    .live {
      display: inline-flex;
      align-items: center;
      gap: 6px;
      font-size: 12.5px;
      color: var(--jv-text-muted);
    }
    .live.on {
      color: var(--jv-danger);
    }
    .dot {
      width: 8px;
      height: 8px;
      border-radius: 50%;
      background: currentcolor;
    }
    .error {
      margin: 0;
      font-size: 12.5px;
      color: var(--jv-danger);
    }
    footer {
      display: flex;
      justify-content: flex-end;
      gap: 8px;
    }
  `,
})
export class CameraDialog {
  private readonly vision = inject(VisionStore);
  private readonly video = viewChild.required<ElementRef<HTMLVideoElement>>('video');
  private stream: MediaStream | null = null;

  readonly closed = output<void>();
  protected readonly streaming = signal(false);
  protected readonly saving = signal(false);
  protected readonly error = signal<string | null>(null);

  constructor() {
    inject(DestroyRef).onDestroy(() => this.release());
    afterNextRender(() => void this.start());
  }

  private async start(): Promise<void> {
    try {
      this.stream = await navigator.mediaDevices.getUserMedia({ video: true, audio: false });
      const video = this.video().nativeElement;
      video.srcObject = this.stream;
      this.streaming.set(true);
    } catch (error) {
      const name = error instanceof DOMException ? error.name : '';
      this.error.set(
        name === 'NotAllowedError'
          ? 'Camera access was denied. Allow it for JARVIS in the system privacy settings.'
          : name === 'NotFoundError'
            ? 'No camera was found.'
            : `The camera could not be started: ${error instanceof Error ? error.message : String(error)}`
      );
    }
  }

  protected async take(): Promise<void> {
    const video = this.video().nativeElement;
    if (!video.videoWidth || !video.videoHeight) {
      return;
    }
    const canvas = document.createElement('canvas');
    canvas.width = video.videoWidth;
    canvas.height = video.videoHeight;
    canvas.getContext('2d')?.drawImage(video, 0, 0);
    const data = canvas.toDataURL('image/jpeg', PHOTO_QUALITY);
    this.release();
    this.saving.set(true);
    await this.vision.attachPhoto(data);
    this.saving.set(false);
    this.closed.emit();
  }

  protected cancel(): void {
    this.release();
    this.closed.emit();
  }

  private release(): void {
    this.stream?.getTracks().forEach((track) => track.stop());
    this.stream = null;
    this.streaming.set(false);
  }
}

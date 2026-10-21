import { Injectable, computed, inject, signal } from '@angular/core';
import { Router } from '@angular/router';
import type { VoicePackage, VoiceState, VoiceStatus, VoiceTranscript } from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';
import { ChatStore } from './chat.store';
import { VisionStore } from './vision.store';

const IDLE: VoiceState = {
  phase: 'idle',
  micOpen: false,
  wakeWordActive: false,
  awaitingCommand: false,
  hearingSpeech: false,
  inputDevice: null,
  lastError: null,
};

/** How long a voice problem stays on screen. */
const NOTICE_MS = 6_000;

/** How long a new spoken request waits for a cancelled reply to end. */
const CANCEL_WAIT_MS = 5_000;

function isActive(phase: string): boolean {
  return phase === 'downloading' || phase === 'verifying' || phase === 'installing';
}

/**
 * Voice state for the whole app: the microphone indicator, push-to-talk,
 * and spoken requests, which are sent to the chat and answered aloud.
 */
@Injectable({ providedIn: 'root' })
export class VoiceStore {
  private readonly api = inject(JarvisApi);
  private readonly chat = inject(ChatStore);
  private readonly vision = inject(VisionStore);
  private readonly router = inject(Router);
  private started = false;
  /** The next push-to-talk result is a microphone test, not a request. */
  private testCapture = false;
  private noticeTimer: ReturnType<typeof setTimeout> | undefined;

  readonly state = signal<VoiceState>(IDLE);
  readonly status = signal<VoiceStatus | null>(null);
  /** Microphone loudness (0–1) while it is open. */
  readonly level = signal(0);
  readonly error = signal<IpcError | null>(null);
  /** Result of the last microphone test. */
  readonly testTranscript = signal<string | null>(null);
  /** The latest voice problem, shown briefly next to the microphone button. */
  readonly notice = signal<string | null>(null);

  readonly micOpen = computed(() => this.state().micOpen);
  readonly phase = computed(() => this.state().phase);
  /** Voice can be offered on this platform. */
  readonly available = computed(() => {
    const status = this.status();
    return status !== null && status.unsupported === null;
  });
  readonly canListen = computed(() => this.status()?.canListen ?? false);
  readonly canSpeak = computed(() => this.status()?.canSpeak ?? false);
  readonly downloads = computed(() => this.status()?.downloads ?? []);
  readonly downloading = computed(() => this.downloads().some((d) => isActive(d.phase)));

  /** Subscribes to the voice engine (app-wide, so the indicator is always right). */
  async start(): Promise<void> {
    if (this.started) {
      return;
    }
    this.started = true;
    try {
      await this.api.onVoiceState((state) => {
        if (state.lastError && state.lastError !== this.state().lastError) {
          this.showNotice(state.lastError);
        }
        this.state.set(state);
        if (!state.micOpen) {
          this.level.set(0);
        }
      });
      await this.api.onVoiceTranscript((transcript) => void this.handleTranscript(transcript));
      await this.api.onVoiceLevel((level) => this.level.set(level));
      await this.refresh();
    } catch (error) {
      this.started = false;
      this.error.set(IpcError.from(error));
    }
  }

  async refresh(): Promise<void> {
    try {
      const status = await this.api.getVoiceStatus();
      this.status.set(status);
      this.state.set(status.state);
    } catch (error) {
      this.error.set(IpcError.from(error));
    }
  }

  /** Opens the microphone for one request; `test` keeps the result here. */
  listen(test = false): Promise<void> {
    this.testCapture = test;
    if (test) {
      this.testTranscript.set(null);
    }
    return this.run(() => this.api.startListening());
  }

  /** The user finished speaking. */
  submit(): Promise<void> {
    return this.run(() => this.api.stopListening(true));
  }

  cancelListening(): Promise<void> {
    this.testCapture = false;
    return this.run(() => this.api.stopListening(false));
  }

  stopSpeaking(): Promise<void> {
    return this.run(() => this.api.stopSpeaking());
  }

  say(text: string): Promise<void> {
    return this.run(() => this.api.speakText(text));
  }

  async install(pkg: VoicePackage): Promise<void> {
    await this.run(() => this.api.installVoicePackage(pkg.id));
    await this.refresh();
  }

  async remove(pkg: VoicePackage): Promise<void> {
    await this.run(() => this.api.removeVoicePackage(pkg.id));
    await this.refresh();
  }

  async cancelDownload(id: string): Promise<void> {
    await this.run(() => this.api.cancelVoiceDownload(id));
    await this.refresh();
  }

  private async handleTranscript(transcript: VoiceTranscript): Promise<void> {
    if (transcript.source === 'pushToTalk' && this.testCapture) {
      this.testCapture = false;
      this.testTranscript.set(transcript.text);
      return;
    }
    // A new spoken request replaces a reply that is still being written.
    if (this.chat.busy()) {
      await this.chat.cancel();
      const deadline = Date.now() + CANCEL_WAIT_MS;
      while (this.chat.busy() && Date.now() < deadline) {
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
    }
    if (!this.router.url.startsWith('/chat')) {
      await this.router.navigateByUrl('/chat');
    }
    await this.chat.send(transcript.text, true, this.vision.take());
  }

  private showNotice(message: string): void {
    this.notice.set(message);
    clearTimeout(this.noticeTimer);
    this.noticeTimer = setTimeout(() => this.notice.set(null), NOTICE_MS);
  }

  private async run(action: () => Promise<unknown>): Promise<void> {
    try {
      await action();
      this.error.set(null);
    } catch (error) {
      const failure = IpcError.from(error);
      this.error.set(failure);
      this.showNotice(failure.message);
    }
  }
}

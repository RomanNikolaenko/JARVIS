import { DestroyRef, Injectable, computed, inject, signal } from '@angular/core';
import { Router } from '@angular/router';
import type { VoicePackage, VoiceState, VoiceStatus, VoiceTranscript } from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';
import { listenerWithDeadline } from '../ipc/listener-deadline';
import { withRequestDeadline } from '../ipc/request-deadline';
import { ChatStore } from './chat.store';
import { VisionStore } from './vision.store';

const IDLE: VoiceState = {
  captureId: null,
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
  private readonly destroyRef = inject(DestroyRef);
  private captureContext: number | null = null;
  private handlingTranscript = false;
  private readonly captures = new Map<string, { context: number; test: boolean } | null>();
  private lastCaptureId: string | null = null;
  private testCaptureId: string | null = null;
  private stateRevision = 0;
  private refreshEpoch = 0;
  private listenEpoch = 0;
  private readonly unlisten: (() => void)[] = [];
  private readonly pendingTranscripts = new Map<
    string,
    { transcript: VoiceTranscript; context: number; timer: ReturnType<typeof setTimeout> }
  >();

  constructor() {
    this.destroyRef.onDestroy(() => {
      clearTimeout(this.noticeTimer);
      this.cleanupListeners();
      for (const pending of this.pendingTranscripts.values()) clearTimeout(pending.timer);
      this.pendingTranscripts.clear();
    });
  }
  private started = false;
  private startEpoch = 0;
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
    if (this.started || this.destroyRef.destroyed) {
      return;
    }
    this.started = true;
    const epoch = ++this.startEpoch;
    try {
      if (
        !(await this.register(
          this.api.onVoiceState((state) => {
            if (this.destroyRef.destroyed || !this.started || epoch !== this.startEpoch) return;
            ++this.stateRevision;
            if (state.captureId && state.captureId !== this.lastCaptureId) {
              this.lastCaptureId = state.captureId;
              if (!this.captures.has(state.captureId)) {
                this.captures.set(state.captureId, {
                  context: this.captureContext ?? this.chat.inputContext(),
                  test: this.testCapture,
                });
                if (this.testCapture) this.testCaptureId = state.captureId;
                this.captureContext = null;
                this.testCapture = false;
                if (this.captures.size > 64) this.captures.delete(this.captures.keys().next().value!);
                const pending = this.pendingTranscripts.get(state.captureId);
                if (pending) {
                  clearTimeout(pending.timer);
                  this.pendingTranscripts.delete(state.captureId);
                  if (pending.context === this.chat.inputContext()) void this.handleTranscript(pending.transcript);
                  else this.captures.set(state.captureId, null);
                }
              }
            }
            if (state.lastError && state.lastError !== this.state().lastError) {
              this.showNotice(state.lastError);
            }
            this.state.set(state);
            if (!state.micOpen) {
              this.level.set(0);
            }
          })
        ))
      )
        return;
      if (
        !(await this.register(
          this.api.onVoiceTranscript((transcript) => {
            if (epoch === this.startEpoch) void this.handleTranscript(transcript);
          })
        ))
      )
        return;
      if (
        !(await this.register(
          this.api.onVoiceLevel((level) => {
            if (!this.destroyRef.destroyed && this.started && epoch === this.startEpoch) this.level.set(level);
          })
        ))
      )
        return;
      await this.refresh();
    } catch (error) {
      this.started = false;
      this.cleanupListeners();
      if (!this.destroyRef.destroyed) this.error.set(IpcError.from(error));
    }
  }

  async refresh(): Promise<void> {
    const epoch = ++this.refreshEpoch;
    const revision = this.stateRevision;
    try {
      const status = await withRequestDeadline(() => this.api.getVoiceStatus(), 10_000, 'Voice status', this.destroyRef);
      if (this.destroyRef.destroyed || epoch !== this.refreshEpoch) return;
      this.status.set(status);
      if (revision === this.stateRevision) this.state.set(status.state);
    } catch (error) {
      if (!this.destroyRef.destroyed && epoch === this.refreshEpoch) this.error.set(IpcError.from(error));
    }
  }

  private async register(registration: Promise<() => void>): Promise<boolean> {
    const stop = await listenerWithDeadline(registration, 10_000, 'Voice subscription', this.destroyRef);
    if (this.destroyRef.destroyed) {
      stop();
      return false;
    }
    this.unlisten.push(stop);
    return true;
  }

  private cleanupListeners(): void {
    for (const stop of this.unlisten.splice(0)) stop();
  }

  /** Opens the microphone for one request; `test` keeps the result here. */
  listen(test = false): Promise<void> {
    const epoch = ++this.listenEpoch;
    this.captureContext = this.chat.inputContext();
    this.testCapture = test;
    if (test) {
      this.testCaptureId = null;
      this.testTranscript.set(null);
    }
    return this.run(async () => {
      try {
        await this.api.startListening();
      } catch (error) {
        if (epoch === this.listenEpoch) {
          this.captureContext = null;
          this.testCapture = false;
          this.testCaptureId = null;
        }
        throw error;
      }
    });
  }

  /** The user finished speaking. */
  submit(): Promise<void> {
    return this.run(() => this.api.stopListening(true));
  }

  cancelListening(): Promise<void> {
    ++this.listenEpoch;
    for (const pending of this.pendingTranscripts.values()) clearTimeout(pending.timer);
    this.pendingTranscripts.clear();
    this.captureContext = -1;
    this.testCapture = false;
    for (const id of this.captures.keys()) this.captures.set(id, null);
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
    if (this.destroyRef.destroyed || !this.started) return;
    if (!this.captures.has(transcript.captureId)) {
      if (this.pendingTranscripts.has(transcript.captureId) || this.pendingTranscripts.size >= 64) return;
      const timer = setTimeout(() => this.pendingTranscripts.delete(transcript.captureId), 5_000);
      this.pendingTranscripts.set(transcript.captureId, { transcript, context: this.chat.inputContext(), timer });
      return;
    }
    const capture = this.captures.get(transcript.captureId);
    if (!capture) return;
    // Consume before awaiting cancellation/navigation so duplicate delivery is harmless.
    this.captures.set(transcript.captureId, null);
    if (this.destroyRef.destroyed || this.handlingTranscript || !transcript.text.trim()) return;
    if (capture.test) {
      if (transcript.captureId === this.testCaptureId) this.testTranscript.set(transcript.text);
      return;
    }
    const context = capture.context;
    const current = () => !this.destroyRef.destroyed && context === this.chat.inputContext();
    if (!current()) return;
    this.handlingTranscript = true;
    try {
      // A new spoken request replaces a reply that is still being written.
      if (this.chat.busy()) {
        await this.chat.cancel();
        const deadline = Date.now() + CANCEL_WAIT_MS;
        while (current() && this.chat.busy() && Date.now() < deadline) {
          await new Promise((resolve) => setTimeout(resolve, 50));
        }
      }
      if (!current()) return;
      if (!this.chat.acceptsInput()) {
        this.showNotice('The chat is not ready. Please repeat your request when it is available.');
        return;
      }
      if (!this.router.url.startsWith('/chat')) {
        if (!(await this.router.navigateByUrl('/chat'))) return;
      }
      if (!current() || !this.chat.acceptsInput()) return;
      await this.chat.send(transcript.text, true, this.vision.take());
    } catch (error) {
      if (current()) this.showNotice(IpcError.from(error).message);
    } finally {
      this.handlingTranscript = false;
    }
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

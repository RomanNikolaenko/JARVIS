import { DestroyRef, Injectable, effect, inject, signal, untracked } from '@angular/core';
import type { MentorCommand, MentorNudge, MentorStatus, VoiceState, VoiceTranscript } from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';
import { AgentsStore } from './agents.store';
import { ChatStore } from './chat.store';
import { VisionStore } from './vision.store';

export const MENTOR_OFF: MentorStatus = {
  conversationId: null,
  projectFolder: null,
  phase: 'off',
  pendingWrite: null,
  typingFile: null,
  lastError: null,
};

/** Tools whose approvals the mentor asks for. */
const MENTOR_TOOLS = new Set(['write_file', 'edit_file']);
const NOTICE_MS = 6_000;

/**
 * Mentor mode in the UI: routes conversation-mode phrases to mentor turns
 * (holding them while a reply runs), runs spoken commands, answers the
 * mentor's write approvals by voice and starts turns when the user saves.
 */
@Injectable({ providedIn: 'root' })
export class MentorStore {
  private readonly api = inject(JarvisApi);
  private readonly chat = inject(ChatStore);
  private readonly vision = inject(VisionStore);
  private readonly agents = inject(AgentsStore);
  private readonly destroyRef = inject(DestroyRef);

  readonly status = signal<MentorStatus>(MENTOR_OFF);
  readonly notice = signal<string | null>(null);
  /** Phrases said while a reply was running, sent together after it. */
  readonly held = signal<readonly string[]>([]);
  /** The start dialog is open. */
  readonly starting = signal(false);

  private readonly speaking = signal(false);
  private lookNext = false;
  private sending = false;
  private lastBusy: boolean | null = null;
  private noticeTimer: ReturnType<typeof setTimeout> | undefined;
  private started = false;
  private readonly stops: (() => void)[] = [];

  constructor() {
    // Held phrases go out once the reply has ended.
    effect(() => {
      const busy = this.chat.busy();
      untracked(() => {
        if (!busy) void this.flushHeld();
      });
    });
    // The core does not speak up about saves while the UI is busy.
    effect(() => {
      const busy = this.chat.busy() || this.speaking() || this.held().length > 0;
      untracked(() => this.reportBusy(busy));
    });
    this.destroyRef.onDestroy(() => {
      for (const stop of this.stops) stop();
      clearTimeout(this.noticeTimer);
    });
  }

  async start(): Promise<void> {
    if (this.started || this.destroyRef.destroyed) return;
    this.started = true;
    try {
      this.stops.push(await this.api.onMentorState((status) => this.status.set(status)));
      this.stops.push(await this.api.onMentorNudge((nudge) => void this.handleNudge(nudge)));
      this.stops.push(await this.api.onVoiceState((state: VoiceState) => this.speaking.set(state.phase === 'speaking')));
      this.status.set(await this.api.mentorStatus());
    } catch (error) {
      this.started = false;
      this.showNotice(IpcError.from(error).message);
    }
  }

  get active(): boolean {
    return this.status().phase !== 'off';
  }

  openStart(): void {
    this.starting.set(true);
  }

  closeStart(): void {
    this.starting.set(false);
  }

  /** Starts a lesson in the open conversation (a new one when none is open). */
  async begin(projectFolder: string): Promise<boolean> {
    try {
      let conversation = this.chat.conversation();
      if (!conversation) {
        conversation = await this.api.createConversation(null, false);
        await this.chat.loadConversations();
        await this.chat.open(conversation.id);
      }
      const selection = this.chat.selection();
      this.status.set(
        await this.api.mentorStart({
          conversationId: conversation.id,
          projectFolder,
          providerId: selection?.providerId ?? null,
          model: selection?.model ?? null,
        })
      );
      this.starting.set(false);
      return true;
    } catch (error) {
      this.showNotice(IpcError.from(error).message);
      return false;
    }
  }

  pause(): Promise<void> {
    return this.run(() => this.api.mentorPause());
  }

  resume(): Promise<void> {
    return this.run(async () => this.status.set(await this.api.mentorResume()));
  }

  end(): Promise<void> {
    this.held.set([]);
    return this.run(() => this.api.mentorStop());
  }

  stopTyping(): Promise<void> {
    return this.run(() => this.api.mentorStopTyping());
  }

  /** The next phrase carries a screenshot. */
  async look(): Promise<void> {
    await this.vision.captureScreen(null);
    this.lookNext = true;
  }

  /** A phrase heard in conversation mode. */
  async handlePhrase(transcript: VoiceTranscript): Promise<void> {
    const text = transcript.text.trim();
    if (!text || !this.active) return;
    const command = await this.command(text);
    if (command && (await this.runCommand(command))) return;
    void this.api.mentorOwnerSpoke().catch(() => undefined);
    if (this.chat.busy() || this.sending) {
      this.held.update((list) => [...list, text]);
      return;
    }
    await this.sendTurn(text, 'phrase');
  }

  /** A push-to-talk phrase while the lesson is paused: "continue" resumes it. */
  async handlePausedPhrase(transcript: VoiceTranscript): Promise<boolean> {
    if (this.status().phase !== 'paused') return false;
    if ((await this.command(transcript.text)) !== 'resume') return false;
    await this.resume();
    return true;
  }

  private async command(text: string): Promise<MentorCommand | null> {
    try {
      return await this.api.mentorCommand(text);
    } catch {
      return null;
    }
  }

  /** Runs a spoken command; false when it is just a phrase for the mentor. */
  private async runCommand(command: MentorCommand): Promise<boolean> {
    switch (command) {
      case 'pause':
        await this.pause();
        return true;
      case 'resume':
        return this.status().phase === 'paused' ? (await this.resume(), true) : false;
      case 'end':
        await this.end();
        return true;
      case 'look':
        await this.look();
        return true;
      case 'stop':
        await this.run(() => this.api.stopSpeaking());
        if (this.status().phase === 'typing') await this.stopTyping();
        return true;
      case 'yes':
      case 'no':
        return this.answerApproval(command === 'yes');
    }
  }

  /** Answers the mentor's pending write by voice; overwriting needs a click. */
  private async answerApproval(approved: boolean): Promise<boolean> {
    const pending = this.agents.approvals().find((a) => a.taskId === null && MENTOR_TOOLS.has(a.tool));
    if (!pending) return false;
    if (approved && pending.risk === 'high') {
      this.showNotice('Overwriting a file needs a click on Approve.');
      return true;
    }
    const error = await this.agents.resolve(pending.id, approved);
    if (error) this.showNotice(error.message);
    return true;
  }

  private async handleNudge(nudge: MentorNudge): Promise<void> {
    const status = this.status();
    if (
      status.phase !== 'listening' ||
      nudge.conversationId !== status.conversationId ||
      this.chat.conversation()?.id !== nudge.conversationId ||
      this.chat.busy() ||
      this.sending ||
      this.held().length > 0
    ) {
      return;
    }
    await this.sendTurn(`[Saved: ${nudge.files.join(', ')}]`, 'filesSaved');
  }

  private async flushHeld(): Promise<void> {
    const held = this.held();
    if (!held.length || this.sending || this.chat.busy()) return;
    this.held.set([]);
    await this.sendTurn(held.join('\n'), 'phrase');
  }

  private async sendTurn(text: string, trigger: 'phrase' | 'filesSaved'): Promise<void> {
    const conversationId = this.status().conversationId;
    if (!conversationId || this.chat.conversation()?.id !== conversationId) {
      this.showNotice('Open the lesson’s conversation in Chat to talk to the mentor.');
      return;
    }
    this.sending = true;
    try {
      const submission = this.lookNext ? this.vision.prepareSend() : null;
      this.lookNext = false;
      const result = await this.chat.send(text, true, submission?.attachments ?? [], undefined, trigger);
      if (result !== 'notSent') submission?.consume();
      if (result === 'notSent') this.showNotice('The mentor did not get that. Please say it again.');
    } catch (error) {
      this.showNotice(IpcError.from(error).message);
    } finally {
      this.sending = false;
      // The reply may have ended before the send returned.
      void this.flushHeld();
    }
  }

  private reportBusy(busy: boolean): void {
    if (!this.active || busy === this.lastBusy) return;
    this.lastBusy = busy;
    void this.api.mentorReportBusy(busy).catch(() => undefined);
  }

  private async run(action: () => Promise<unknown>): Promise<void> {
    try {
      await action();
    } catch (error) {
      this.showNotice(IpcError.from(error).message);
    }
  }

  private showNotice(message: string): void {
    this.notice.set(message);
    clearTimeout(this.noticeTimer);
    this.noticeTimer = setTimeout(() => this.notice.set(null), NOTICE_MS);
  }
}

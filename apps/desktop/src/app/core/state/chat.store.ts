import { Injectable, computed, inject, signal } from '@angular/core';
import type {
  CaptureInfo,
  ChatStreamEvent,
  ContextReport,
  Conversation,
  ConversationPatch,
  ConversationSummary,
  StoredMessage,
} from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';
import { type ModelOption, ModelsStore, optionKey } from './models.store';
import { SettingsStore } from './settings.store';

export type ChatEntryStatus = 'complete' | 'streaming' | 'cancelled' | 'failed';

export interface ChatEntry {
  /** Message id for saved messages, a local id while sending. */
  readonly id: string;
  readonly role: 'user' | 'assistant';
  readonly content: string;
  readonly status: ChatEntryStatus;
  readonly model?: string;
  readonly error?: string;
  readonly elapsedMs?: number;
  /** How the core built the context of this reply. */
  readonly context?: ContextReport;
  readonly createdAtMs?: number;
  /** Images sent with this message (previews, this session only). */
  readonly images?: readonly CaptureInfo[];
}

/** `connecting`: request sent, waiting for `started`; `streaming`: receiving text. */
export type ChatPhase = 'idle' | 'connecting' | 'streaming';

export const MAX_MESSAGE_LENGTH = 32_000;

function toEntry(message: StoredMessage): ChatEntry {
  return {
    id: message.id,
    role: message.role,
    content: message.content,
    status: message.status === 'cancelled' ? 'cancelled' : 'complete',
    model: message.model ?? undefined,
    createdAtMs: message.createdAtMs,
  };
}

/**
 * Conversations and the open chat. The core owns the history: it builds the
 * context (memory, summaries, token budget) and saves both sides of a turn;
 * the UI only sends the new message.
 */
@Injectable({ providedIn: 'root' })
export class ChatStore {
  private readonly api = inject(JarvisApi);
  private readonly models = inject(ModelsStore);
  private readonly settings = inject(SettingsStore);
  private localId = 0;
  private cancelRequested = false;
  /** `null` = automatic: the core's model router decides. */
  private readonly selectedKey = signal<string | null>(null);

  readonly conversations = signal<readonly Conversation[]>([]);
  readonly conversationQuery = signal('');
  /** The open conversation; `null` for a new chat without messages yet. */
  readonly conversation = signal<Conversation | null>(null);
  readonly summary = signal<ConversationSummary | null>(null);
  /** Settings of the next new chat. */
  readonly temporaryDraft = signal(false);
  readonly draftProjectId = signal<string | null>(null);
  readonly entries = signal<readonly ChatEntry[]>([]);
  readonly phase = signal<ChatPhase>('idle');
  readonly activeRequestId = signal<string | null>(null);
  readonly error = signal<IpcError | null>(null);
  readonly loading = signal(false);

  readonly busy = computed(() => this.phase() !== 'idle');
  readonly savingEnabled = computed(() => this.settings.settings()?.memory.enabled ?? true);
  /** The open (or next) chat is not saved. */
  readonly temporary = computed(() => this.conversation()?.temporary ?? (this.temporaryDraft() || !this.savingEnabled()));
  /** A saved conversation cannot continue while saving is off. */
  readonly readOnly = computed(() => {
    const conversation = this.conversation();
    return !!conversation && !conversation.temporary && !this.savingEnabled();
  });
  readonly projectId = computed(() => (this.conversation() ? this.conversation()!.projectId : this.draftProjectId()));

  readonly modelOptions = this.models.chatOptions;
  readonly modelsError = this.models.error;
  readonly automaticRoute = this.models.generalRoute;
  /** Explicitly selected model; `null` when automatic or no longer available. */
  readonly selected = computed<ModelOption | null>(() => {
    const key = this.selectedKey();
    return key === null ? null : (this.modelOptions().find((o) => optionKey(o.provider.id, o.model.id) === key) ?? null);
  });
  /** True when models are loaded and no real AI model is reachable. */
  readonly diagnosticOnly = computed(() => this.models.loaded() && !this.models.realModelAvailable());

  loadModels(force = false): Promise<void> {
    return this.models.refresh(force);
  }

  /** Selects a model, or automatic routing with `null`. */
  select(option: ModelOption | null): void {
    this.selectedKey.set(option ? optionKey(option.provider.id, option.model.id) : null);
  }

  async loadConversations(): Promise<void> {
    try {
      const query = this.conversationQuery().trim();
      this.conversations.set(await this.api.listConversations(query || undefined));
      this.error.set(null);
    } catch (error) {
      this.error.set(IpcError.from(error));
    }
  }

  /** Opens a conversation. Ignored while a reply is running. */
  async open(id: string): Promise<void> {
    if (this.busy()) {
      return;
    }
    this.loading.set(true);
    try {
      const detail = await this.api.getConversation(id);
      this.conversation.set(detail.conversation);
      this.summary.set(detail.summary);
      this.entries.set(detail.messages.map(toEntry));
      this.error.set(null);
    } catch (error) {
      this.error.set(IpcError.from(error));
    } finally {
      this.loading.set(false);
    }
  }

  /** Starts a new chat; it is created when the first message is sent. */
  startNew(temporary = false, projectId: string | null = null): void {
    if (this.busy()) {
      return;
    }
    this.conversation.set(null);
    this.summary.set(null);
    this.entries.set([]);
    this.temporaryDraft.set(temporary);
    this.draftProjectId.set(projectId);
    this.error.set(null);
  }

  /**
   * Sends a message; `speak` reads the reply aloud (spoken requests) and
   * `attachments` are captures shown to a vision model.
   */
  async send(text: string, speak = false, attachments: readonly CaptureInfo[] = []): Promise<void> {
    const content = text.trim();
    if (!content || this.busy() || content.length > MAX_MESSAGE_LENGTH || this.readOnly()) {
      return;
    }
    this.phase.set('connecting');
    this.cancelRequested = false;
    this.addEntry({ role: 'user', content, status: 'complete', images: attachments.length ? attachments : undefined });
    const assistantId = this.addEntry({ role: 'assistant', content: '', status: 'streaming' }).id;

    const selection = this.selected();
    try {
      let conversation = this.conversation();
      if (!conversation) {
        conversation = await this.api.createConversation(this.draftProjectId(), this.temporaryDraft());
        this.conversation.set(conversation);
      }
      const requestId = await this.api.sendMessage(
        {
          conversationId: conversation.id,
          content,
          providerId: selection?.provider.id ?? null,
          model: selection?.model.id ?? null,
          temperature: null,
          maxOutputTokens: null,
          speak,
          attachments: attachments.map((a) => a.id),
        },
        (event) => this.onEvent(assistantId, event)
      );
      if (this.phase() !== 'idle' && this.activeRequestId() === null) {
        this.activeRequestId.set(requestId);
      }
      if (this.cancelRequested && this.phase() !== 'idle') {
        await this.cancel();
      }
    } catch (error) {
      this.updateEntry(assistantId, { status: 'failed', error: IpcError.from(error).message });
      this.finish();
    }
  }

  /** Requests cancellation of the running request. */
  async cancel(): Promise<void> {
    const requestId = this.activeRequestId();
    if (!requestId) {
      // `started` has not arrived yet; cancel as soon as the id is known.
      this.cancelRequested = this.busy();
      return;
    }
    try {
      await this.api.cancelChat(requestId);
    } catch (error) {
      console.error('Failed to cancel chat request', error);
    }
  }

  async update(id: string, patch: ConversationPatch): Promise<boolean> {
    try {
      const updated = await this.api.updateConversation(id, patch);
      if (this.conversation()?.id === id) {
        this.conversation.set(updated);
      }
      this.conversations.update((list) => list.map((c) => (c.id === id ? updated : c)));
      this.error.set(null);
      return true;
    } catch (error) {
      this.error.set(IpcError.from(error));
      return false;
    }
  }

  /** Moves the open (or next) chat into a project. */
  async setProject(projectId: string | null): Promise<void> {
    const conversation = this.conversation();
    if (!conversation) {
      this.draftProjectId.set(projectId);
      return;
    }
    await this.update(conversation.id, {
      title: null,
      projectId,
      clearProject: projectId === null,
      pinnedNote: null,
    });
  }

  rename(id: string, title: string): Promise<boolean> {
    return this.update(id, { title, projectId: null, clearProject: false, pinnedNote: null });
  }

  /** Sets the pinned note (working memory); creates the chat if needed. */
  async setPinnedNote(note: string): Promise<boolean> {
    let conversation = this.conversation();
    if (!conversation) {
      try {
        conversation = await this.api.createConversation(this.draftProjectId(), this.temporaryDraft());
        this.conversation.set(conversation);
      } catch (error) {
        this.error.set(IpcError.from(error));
        return false;
      }
    }
    const saved = await this.update(conversation.id, {
      title: null,
      projectId: null,
      clearProject: false,
      pinnedNote: note,
    });
    await this.loadConversations();
    return saved;
  }

  async remove(id: string): Promise<void> {
    try {
      await this.api.deleteConversation(id);
      if (this.conversation()?.id === id) {
        this.startNew();
      }
      this.conversations.update((list) => list.filter((c) => c.id !== id));
      this.error.set(null);
    } catch (error) {
      this.error.set(IpcError.from(error));
    }
  }

  private onEvent(entryId: string, event: ChatStreamEvent): void {
    switch (event.type) {
      case 'started':
        this.activeRequestId.set(event.requestId);
        this.phase.set('streaming');
        this.updateEntry(entryId, { model: event.model, context: event.context ?? undefined });
        if (this.cancelRequested) {
          void this.cancel();
        }
        break;
      case 'delta':
        this.appendContent(entryId, event.text);
        break;
      case 'finished':
        this.updateEntry(entryId, {
          status: event.finishReason === 'cancelled' ? 'cancelled' : 'complete',
          elapsedMs: event.elapsedMs,
        });
        this.finish();
        break;
      case 'failed':
        this.updateEntry(entryId, {
          status: 'failed',
          error: event.error.message,
          elapsedMs: event.elapsedMs,
        });
        this.finish();
        break;
    }
  }

  private finish(): void {
    this.phase.set('idle');
    this.activeRequestId.set(null);
    this.cancelRequested = false;
    void this.refreshAfterTurn();
  }

  /** Picks up the title the core derived from the first message. */
  private async refreshAfterTurn(): Promise<void> {
    await this.loadConversations();
    const id = this.conversation()?.id;
    const fresh = this.conversations().find((c) => c.id === id);
    if (fresh) {
      this.conversation.set(fresh);
    }
  }

  private addEntry(entry: Omit<ChatEntry, 'id'>): ChatEntry {
    const created: ChatEntry = { ...entry, id: `local-${++this.localId}` };
    this.entries.update((list) => [...list, created]);
    return created;
  }

  private updateEntry(id: string, patch: Partial<Omit<ChatEntry, 'id'>>): void {
    this.entries.update((list) => list.map((e) => (e.id === id ? { ...e, ...patch } : e)));
  }

  private appendContent(id: string, text: string): void {
    this.entries.update((list) => list.map((e) => (e.id === id ? { ...e, content: e.content + text } : e)));
  }
}

import { DestroyRef, Injectable, computed, inject, signal } from '@angular/core';
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
import { withRequestDeadline } from '../ipc/request-deadline';
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
export const CONVERSATION_LIST_TIMEOUT_MS = 30_000;
export const CONVERSATION_OPEN_TIMEOUT_MS = 30_000;
export const CHAT_CANCEL_TIMEOUT_MS = 30_000;

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
  private readonly destroyRef = inject(DestroyRef);
  private activeEntryId: string | null = null;
  private openRequest = 0;
  private openingId: string | null = null;
  private listRequest = 0;
  private readonly conversationWrites = new Set<string>();
  private readonly models = inject(ModelsStore);
  private readonly settings = inject(SettingsStore);
  private localId = 0;
  private cancelRequested = false;
  private cancellation: { entryId: string; promise: Promise<void> } | null = null;
  /** `null` = automatic: the core's model router decides. */
  private readonly selectedKey = signal<string | null>(null);

  readonly conversations = signal<readonly Conversation[]>([]);
  readonly conversationQuery = signal('');
  readonly conversationsLoading = signal(false);
  readonly conversationsError = signal<IpcError | null>(null);
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
  readonly cancelError = signal<IpcError | null>(null);
  readonly cancelling = signal(false);
  readonly loading = signal(false);
  readonly savingNote = signal(false);
  readonly deletingIds = signal<readonly string[]>([]);
  readonly noteCreationId = signal<string | null>(null);

  constructor() {
    this.destroyRef.onDestroy(() => {
      this.activeEntryId = null;
      this.resetCancellation();
      ++this.openRequest;
      this.loading.set(false);
    });
  }

  /** The native turn may still be running; never automatically resend it. */
  connectionLost(error: IpcError): void {
    const id = this.activeEntryId;
    if (id === null || this.destroyRef.destroyed) return;
    this.updateEntry(id, {
      status: 'failed',
      error: `Connection lost: ${error.message} The reply may still be running. No message was resent.${this.conversation() && !this.temporary() ? ' Reopen this conversation after reconnecting to check its saved result.' : ''}`,
    });
    this.activeEntryId = null;
    this.phase.set('idle');
    this.activeRequestId.set(null);
    this.cancelRequested = false;
    this.resetCancellation();
  }

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

  /** Invalidate immediately, including the debounce interval and A → B → A edits. */
  setConversationQuery(query: string): void {
    if (this.destroyRef.destroyed || query === this.conversationQuery()) return;
    this.conversationQuery.set(query);
    this.invalidateConversationList();
    this.conversations.set([]);
    this.conversationsError.set(null);
  }

  private invalidateConversationList(): void {
    ++this.listRequest;
    this.conversationsLoading.set(false);
  }

  async loadConversations(): Promise<void> {
    if (this.destroyRef.destroyed) return;
    const request = ++this.listRequest;
    const query = this.conversationQuery().trim();
    const current = () => request === this.listRequest && query === this.conversationQuery().trim() && !this.destroyRef.destroyed;
    this.conversationsLoading.set(true);
    this.conversationsError.set(null);
    try {
      const conversations = await withRequestDeadline(
        () => this.api.listConversations(query || undefined),
        CONVERSATION_LIST_TIMEOUT_MS,
        'Conversation list',
        this.destroyRef
      );
      if (!current()) return;
      this.conversations.set(conversations);
    } catch (error) {
      if (current()) this.conversationsError.set(IpcError.from(error));
    } finally {
      if (request === this.listRequest) this.conversationsLoading.set(false);
    }
  }

  /** Opens a conversation. Ignored while a reply is running. */
  async open(id: string): Promise<void> {
    if (this.busy() || this.destroyRef.destroyed) {
      return;
    }
    this.loading.set(true);
    this.noteCreationId.set(null);
    this.error.set(null);
    const request = ++this.openRequest;
    this.openingId = id;
    try {
      const detail = await withRequestDeadline(
        () => this.api.getConversation(id),
        CONVERSATION_OPEN_TIMEOUT_MS,
        'Conversation',
        this.destroyRef
      );
      if (request !== this.openRequest || this.destroyRef.destroyed) return;
      this.conversation.set(detail.conversation);
      this.summary.set(detail.summary);
      this.entries.set(detail.messages.map(toEntry));
      this.error.set(null);
    } catch (error) {
      if (request === this.openRequest && !this.destroyRef.destroyed) this.error.set(IpcError.from(error));
    } finally {
      if (request === this.openRequest) {
        this.loading.set(false);
        this.openingId = null;
      }
    }
  }

  /** Starts a new chat; it is created when the first message is sent. */
  startNew(temporary = false, projectId: string | null = null): void {
    if (this.busy() || this.destroyRef.destroyed) {
      return;
    }
    ++this.openRequest;
    this.openingId = null;
    this.noteCreationId.set(null);
    this.loading.set(false);
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
    if (this.deletingIds().includes(this.conversation()?.id ?? '')) return;
    const content = text.trim();
    if (
      !content ||
      this.busy() ||
      this.savingNote() ||
      this.destroyRef.destroyed ||
      content.length > MAX_MESSAGE_LENGTH ||
      this.readOnly()
    ) {
      return;
    }
    this.phase.set('connecting');
    this.resetCancellation();
    ++this.openRequest;
    this.openingId = null;
    this.loading.set(false);
    this.cancelRequested = false;
    this.addEntry({ role: 'user', content, status: 'complete', images: attachments.length ? attachments : undefined });
    const assistantId = this.addEntry({ role: 'assistant', content: '', status: 'streaming' }).id;
    this.activeEntryId = assistantId;
    const current = () => this.activeEntryId === assistantId && !this.destroyRef.destroyed;

    const selection = this.selected();
    try {
      let conversation = this.conversation();
      if (!conversation) {
        conversation = await this.api.createConversation(this.draftProjectId(), this.temporaryDraft());
        if (!current()) return;
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
      if (!current()) return;
      if (this.phase() !== 'idle' && this.activeRequestId() === null) {
        this.activeRequestId.set(requestId);
      }
      if (this.cancelRequested && this.phase() !== 'idle') {
        await this.cancel();
      }
    } catch (error) {
      if (!current()) return;
      this.updateEntry(assistantId, { status: 'failed', error: IpcError.from(error).message });
      this.finish();
    }
  }

  /** Requests cancellation of the running request. */
  cancel(): Promise<void> {
    const entryId = this.activeEntryId;
    if (entryId === null || this.destroyRef.destroyed) return Promise.resolve();
    if (this.cancellation?.entryId === entryId) return this.cancellation.promise;
    const requestId = this.activeRequestId();
    if (!requestId) {
      // `started` has not arrived yet; cancel as soon as the id is known.
      this.cancelRequested = this.busy();
      return Promise.resolve();
    }
    this.cancelRequested = false;
    this.cancelling.set(true);
    this.cancelError.set(null);
    const current = () => this.activeEntryId === entryId && !this.destroyRef.destroyed;
    const promise = withRequestDeadline(
      () => this.api.cancelChat(requestId),
      CHAT_CANCEL_TIMEOUT_MS,
      'Stopping the reply',
      this.destroyRef
    )
      .then((accepted) => {
        if (current() && !accepted)
          this.cancelError.set(
            new IpcError('unavailable', 'The request is no longer active in the core. Waiting for its final result.')
          );
      })
      .catch((error: unknown) => {
        if (current()) this.cancelError.set(IpcError.from(error));
      })
      .finally(() => {
        if (this.cancellation?.promise === promise) {
          this.cancellation = null;
          this.cancelling.set(false);
        }
      });
    this.cancellation = { entryId, promise };
    return promise;
  }

  private resetCancellation(): void {
    this.cancellation = null;
    this.cancelling.set(false);
    this.cancelError.set(null);
  }

  async update(id: string, patch: ConversationPatch): Promise<boolean> {
    if (this.destroyRef.destroyed || this.conversationWrites.has(id)) return false;
    this.conversationWrites.add(id);
    const context = this.openRequest;
    try {
      const updated = await this.api.updateConversation(id, patch);
      if (this.destroyRef.destroyed) return false;
      this.invalidateConversationList();
      if (context === this.openRequest && this.conversation()?.id === id) {
        this.conversation.set(updated);
      }
      this.conversations.update((list) => list.map((c) => (c.id === id ? updated : c)));
      if (context === this.openRequest) this.error.set(null);
      return true;
    } catch (error) {
      if (context === this.openRequest && !this.destroyRef.destroyed) this.error.set(IpcError.from(error));
      return false;
    } finally {
      this.conversationWrites.delete(id);
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
    if (this.destroyRef.destroyed || this.savingNote() || this.busy() || this.readOnly()) return false;
    this.savingNote.set(true);
    this.noteCreationId.set(null);
    const context = this.openRequest;
    try {
      let conversation = this.conversation();
      if (!conversation) {
        try {
          conversation = await this.api.createConversation(this.draftProjectId(), this.temporaryDraft());
          if (context !== this.openRequest || this.destroyRef.destroyed) return false;
          this.noteCreationId.set(conversation.id);
          this.conversation.set(conversation);
        } catch (error) {
          if (context === this.openRequest && !this.destroyRef.destroyed) this.error.set(IpcError.from(error));
          return false;
        }
      }
      const saved = await this.update(conversation.id, {
        title: null,
        projectId: null,
        clearProject: false,
        pinnedNote: note,
      });
      if (saved) await this.loadConversations();
      return saved && context === this.openRequest && !this.destroyRef.destroyed;
    } finally {
      this.savingNote.set(false);
    }
  }

  async remove(id: string): Promise<'closed' | 'deleted' | false> {
    if (this.destroyRef.destroyed || this.conversationWrites.has(id) || (this.busy() && this.conversation()?.id === id))
      return false;
    this.conversationWrites.add(id);
    this.deletingIds.update((ids) => [...ids, id]);
    const context = this.openRequest;
    try {
      await this.api.deleteConversation(id);
      if (this.destroyRef.destroyed) return false;
      this.invalidateConversationList();
      const closed = context === this.openRequest && this.conversation()?.id === id;
      // Prevent an already-dispatched read from reopening the deleted record.
      if (this.openingId === id) {
        ++this.openRequest;
        this.openingId = null;
        this.loading.set(false);
      }
      if (this.conversation()?.id === id) {
        if (closed) this.startNew();
        else {
          // Another open may be pending; retain its request generation.
          this.conversation.set(null);
          this.summary.set(null);
          this.entries.set([]);
        }
      }
      this.conversations.update((list) => list.filter((c) => c.id !== id));
      if (closed || context === this.openRequest) this.error.set(null);
      return closed ? 'closed' : 'deleted';
    } catch (error) {
      if (context === this.openRequest && !this.destroyRef.destroyed) this.error.set(IpcError.from(error));
      return false;
    } finally {
      this.conversationWrites.delete(id);
      this.deletingIds.update((ids) => ids.filter((entry) => entry !== id));
    }
  }

  private onEvent(entryId: string, event: ChatStreamEvent): void {
    if (entryId !== this.activeEntryId || this.destroyRef.destroyed) return;
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
    this.resetCancellation();
    this.activeEntryId = null;
    this.phase.set('idle');
    this.activeRequestId.set(null);
    this.cancelRequested = false;
    void this.refreshAfterTurn();
  }

  /** Picks up the title the core derived from the first message. */
  private async refreshAfterTurn(): Promise<void> {
    const refresh = this.loadConversations();
    const request = this.listRequest;
    await refresh;
    if (this.destroyRef.destroyed || request !== this.listRequest || this.conversationsError()) return;
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

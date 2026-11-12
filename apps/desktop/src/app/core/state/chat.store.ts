import { DestroyRef, Injectable, computed, inject, signal } from '@angular/core';
import type {
  CaptureInfo,
  ChatStreamEvent,
  ContextReport,
  Conversation,
  ConversationDetail,
  ConversationPatch,
  ConversationSummary,
  ErrorCode,
  ModelRef,
  StoredMessage,
} from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';
import { withRequestDeadline } from '../ipc/request-deadline';
import { type ModelOption, ModelsStore } from './models.store';
import { SettingsStore } from './settings.store';
import { VisionStore } from './vision.store';

export type ChatEntryStatus = 'complete' | 'streaming' | 'cancelled' | 'failed';

export interface ChatEntry {
  /** Message id for saved messages, a local id while sending. */
  readonly id: string;
  readonly role: 'user' | 'assistant';
  readonly content: string;
  readonly status: ChatEntryStatus;
  readonly model?: string;
  readonly error?: string;
  /** Category of `error`, when the core reported one. */
  readonly errorCode?: ErrorCode;
  /** The native turn may still be running; a saved snapshot cannot prove its outcome. */
  readonly outcomeUnknown?: boolean;
  readonly elapsedMs?: number;
  /** How the core built the context of this reply. */
  readonly context?: ContextReport;
  readonly createdAtMs?: number;
  /** Images sent with this message (previews, this session only). */
  readonly images?: readonly CaptureInfo[];
}

/** `connecting`: request sent, waiting for `started`; `streaming`: receiving text. */
export type ChatPhase = 'idle' | 'connecting' | 'streaming';
/** Whether the core admitted a turn; an unknown result must never be retried automatically. */
export type ChatSendResult = 'accepted' | 'notSent' | 'unknown';

export const MAX_MESSAGE_LENGTH = 32_000;
export const CONVERSATION_LIST_TIMEOUT_MS = 30_000;
export const CONVERSATION_OPEN_TIMEOUT_MS = 30_000;
export const CHAT_CANCEL_TIMEOUT_MS = 30_000;
export const CHAT_SEND_TIMEOUT_MS = 30_000;
export const CHAT_NOTE_SAVE_TIMEOUT_MS = 30_000;

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
  private readonly vision = inject(VisionStore);
  private readonly destroyRef = inject(DestroyRef);
  private readonly disposed = signal(false);
  private activeEntryId: string | null = null;
  private activeSend: {
    entryId: string;
    dispatched: boolean;
    settle: (result: ChatSendResult) => ChatSendResult;
  } | null = null;
  private openRequest = 0;
  private openingId: string | null = null;
  private listRequest = 0;
  private savedMessagesRequest = 0;
  private savedMessagesInFlight: Promise<void> | null = null;
  private noteSave: { cancel: () => void } | null = null;
  private readonly conversationWrites = new Set<string>();
  private readonly models = inject(ModelsStore);
  private readonly settings = inject(SettingsStore);
  private localId = 0;
  private cancelRequested = false;
  private cancellation: { entryId: string; promise: Promise<void> } | null = null;
  /** `null` = automatic: the core's model router decides. */
  private readonly selectedModel = signal<ModelRef | null>(null);
  private selectionError: IpcError | null = null;

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
  readonly inputContext = signal(0);
  /** A committed switch of editors; creating the current conversation preserves its draft. */
  readonly editorContext = signal(0);
  readonly acceptsInput = computed(
    () =>
      !this.disposed() &&
      !this.destroyRef.destroyed &&
      !this.busy() &&
      !this.loading() &&
      !this.savingNote() &&
      !this.readOnly() &&
      !this.selectionUnavailable() &&
      !this.deletingIds().includes(this.conversation()?.id ?? '')
  );
  readonly deletingIds = signal<readonly string[]>([]);
  readonly noteCreationId = signal<string | null>(null);
  /** Separate from the local transcript, which may contain unsaved partial text. */
  readonly savedMessages = signal<ConversationDetail | null>(null);
  readonly checkingSavedMessages = signal(false);
  readonly savedMessagesError = signal<IpcError | null>(null);
  readonly canCheckSaved = computed(() => {
    const conversation = this.conversation();
    return (
      !this.disposed() &&
      !this.destroyRef.destroyed &&
      !!conversation &&
      !conversation.temporary &&
      !this.busy() &&
      !this.loading() &&
      !this.savingNote() &&
      !this.deletingIds().includes(conversation.id) &&
      this.entries().some((entry) => entry.outcomeUnknown)
    );
  });

  constructor() {
    this.destroyRef.onDestroy(() => {
      this.disposed.set(true);
      this.invalidateNoteSave();
      this.invalidateSavedMessages();
      this.activeSend?.settle(this.activeSend.dispatched ? 'unknown' : 'notSent');
      this.activeEntryId = null;
      this.resetCancellation();
      ++this.openRequest;
      this.loading.set(false);
    });
  }

  /** The native turn may still be running; never automatically resend it. */
  connectionLost(error: IpcError): void {
    if (this.destroyRef.destroyed) return;
    this.invalidateNoteSave();
    this.invalidateSavedMessages();
    const id = this.activeEntryId;
    if (id === null) return;
    const notSent = this.activeSend?.entryId === id && !this.activeSend.dispatched;
    if (this.activeSend?.entryId === id) {
      this.activeSend.settle(this.activeSend.dispatched ? 'unknown' : 'notSent');
    }
    this.updateEntry(id, {
      status: 'failed',
      outcomeUnknown: !notSent,
      error: notSent
        ? `Connection lost: ${error.message} The message was not sent.`
        : `Connection lost: ${error.message} The reply may still be running. No message was resent.${this.conversation() && !this.temporary() ? ' Use Check saved messages after reconnecting to inspect the saved conversation.' : ''}`,
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
  /** The user's explicit choice is retained even when discovery loses it. */
  readonly selection = this.selectedModel.asReadonly();
  /** Live metadata; unavailable explicit selections have no current option. */
  readonly selected = computed<ModelOption | null>(() => {
    const choice = this.selection();
    return choice === null
      ? null
      : (this.modelOptions().find((option) => option.provider.id === choice.providerId && option.model.id === choice.model) ??
          null);
  });
  readonly selectionUnavailable = computed(() => this.selection() !== null && this.selected() === null);
  /** True when models are loaded and no real AI model is reachable. */
  readonly diagnosticOnly = computed(() => this.models.loaded() && !this.models.realModelAvailable());

  loadModels(force = false): Promise<void> {
    return this.models.refresh(force);
  }

  /** Selects a model, or automatic routing with `null`. */
  select(option: ModelOption | null): void {
    this.clearSelectionError();
    this.selectedModel.set(option ? Object.freeze({ providerId: option.provider.id, model: option.model.id }) : null);
  }

  private clearSelectionError(): void {
    if (this.selectionError !== null && this.error() === this.selectionError) this.error.set(null);
    this.selectionError = null;
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

  private invalidateSavedMessages(): void {
    ++this.savedMessagesRequest;
    this.savedMessagesInFlight = null;
    this.checkingSavedMessages.set(false);
    this.savedMessages.set(null);
    this.savedMessagesError.set(null);
  }

  private invalidateNoteSave(): void {
    const pending = this.noteSave;
    this.noteSave = null;
    this.savingNote.set(false);
    pending?.cancel();
  }

  /** Reads saved messages without replacing local text or repeating the uncertain turn. */
  checkSavedMessages(): Promise<void> {
    if (this.destroyRef.destroyed || !this.canCheckSaved()) return Promise.resolve();
    if (this.savedMessagesInFlight) return this.savedMessagesInFlight;
    const id = this.conversation()!.id;
    const context = this.openRequest;
    const request = ++this.savedMessagesRequest;
    const current = () =>
      request === this.savedMessagesRequest &&
      context === this.openRequest &&
      this.conversation()?.id === id &&
      !this.destroyRef.destroyed;
    this.checkingSavedMessages.set(true);
    this.savedMessagesError.set(null);
    const promise = withRequestDeadline(
      () => this.api.getConversation(id),
      CONVERSATION_OPEN_TIMEOUT_MS,
      'Checking saved messages',
      this.destroyRef
    )
      .then((detail) => {
        if (!current()) return;
        if (detail.conversation.id !== id || detail.conversation.temporary) {
          throw new IpcError('internal', 'The saved messages do not belong to this conversation.');
        }
        this.savedMessages.set(detail);
      })
      .catch((error: unknown) => {
        if (current()) this.savedMessagesError.set(IpcError.from(error));
      })
      .finally(() => {
        if (request === this.savedMessagesRequest) {
          this.checkingSavedMessages.set(false);
          this.savedMessagesInFlight = null;
        }
      });
    this.savedMessagesInFlight = promise;
    return promise;
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
    this.invalidateNoteSave();
    this.invalidateSavedMessages();
    this.loading.set(true);
    this.inputContext.update((context) => context + 1);
    this.vision.clear();
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
      this.editorContext.update((context) => context + 1);
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
    this.invalidateNoteSave();
    this.invalidateSavedMessages();
    this.editorContext.update((context) => context + 1);
    ++this.openRequest;
    this.inputContext.update((context) => context + 1);
    this.vision.clear();
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
  async send(
    text: string,
    speak = false,
    attachments: readonly CaptureInfo[] = [],
    onResult?: (result: ChatSendResult) => void
  ): Promise<ChatSendResult> {
    let result: ChatSendResult | null = null;
    const settle = (next: ChatSendResult): ChatSendResult => {
      if (result === null) {
        result = next;
        onResult?.(next);
      }
      return result;
    };
    if (this.deletingIds().includes(this.conversation()?.id ?? '')) return settle('notSent');
    const content = text.trim();
    if (
      !content ||
      this.busy() ||
      this.loading() ||
      this.savingNote() ||
      this.destroyRef.destroyed ||
      content.length > MAX_MESSAGE_LENGTH ||
      this.readOnly()
    ) {
      return settle('notSent');
    }
    this.clearSelectionError();
    if (this.selectionUnavailable()) {
      this.selectionError = new IpcError(
        'unavailable',
        'The selected model is unavailable. Choose another model or select Automatic before sending.'
      );
      this.error.set(this.selectionError);
      return settle('notSent');
    }
    // Snapshot the explicit choice before creating a conversation. Discovery
    // changes or a selection for the next turn must not reroute this message.
    const selection = this.selection();
    this.invalidateSavedMessages();
    this.phase.set('connecting');
    this.resetCancellation();
    ++this.openRequest;
    this.openingId = null;
    this.loading.set(false);
    this.cancelRequested = false;
    this.addEntry({ role: 'user', content, status: 'complete', images: attachments.length ? attachments : undefined });
    const assistantId = this.addEntry({ role: 'assistant', content: '', status: 'streaming' }).id;
    this.activeEntryId = assistantId;
    const submission = { entryId: assistantId, dispatched: false, settle };
    this.activeSend = submission;
    const current = () => this.activeEntryId === assistantId && !this.destroyRef.destroyed;

    try {
      let conversation = this.conversation();
      if (!conversation) {
        conversation = await withRequestDeadline(
          () => this.api.createConversation(this.draftProjectId(), this.temporaryDraft()),
          CHAT_SEND_TIMEOUT_MS,
          'Creating the conversation',
          this.destroyRef
        );
        if (!current()) return settle('notSent');
        this.conversation.set(conversation);
      }
      const conversationId = conversation.id;
      const requestId = await withRequestDeadline(
        () => {
          submission.dispatched = true;
          return this.api.sendMessage(
            {
              conversationId,
              content,
              providerId: selection?.providerId ?? null,
              model: selection?.model ?? null,
              temperature: null,
              maxOutputTokens: null,
              speak,
              attachments: attachments.map((a) => a.id),
            },
            (event) => {
              // An event proves admission even if it finishes before invoke replies.
              settle('accepted');
              this.onEvent(assistantId, event);
            }
          );
        },
        CHAT_SEND_TIMEOUT_MS,
        'Sending the message',
        this.destroyRef
      );
      settle('accepted');
      if (!current()) return settle('accepted');
      if (this.phase() !== 'idle' && this.activeRequestId() === null) {
        this.activeRequestId.set(requestId);
      }
      if (this.cancelRequested && this.phase() !== 'idle') {
        await this.cancel();
      }
      return settle('accepted');
    } catch (error) {
      const failure = IpcError.from(error);
      const outcome = settle(
        !submission.dispatched ||
          failure.origin === 'notDispatched' ||
          (failure.origin === 'core' && failure.code !== 'io' && failure.code !== 'internal')
          ? 'notSent'
          : 'unknown'
      );
      // Once a stream proves admission, a lost invoke acknowledgement cannot end it.
      if (outcome === 'accepted' || !current()) return outcome;
      const message =
        outcome === 'unknown'
          ? `${failure.message} The request may still be running. No message was resent.${this.conversation() && !this.temporary() ? ' Use Check saved messages after reconnecting to inspect the saved conversation.' : ''}`
          : failure.message;
      this.updateEntry(assistantId, {
        status: 'failed',
        error: message,
        errorCode: failure.code,
        outcomeUnknown: outcome === 'unknown',
      });
      this.finish();
      return outcome;
    } finally {
      if (this.activeSend === submission) this.activeSend = null;
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
    if (this.loading() || this.savingNote() || this.busy() || this.destroyRef.destroyed) return;
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
    if (this.destroyRef.destroyed || this.loading() || this.savingNote() || this.busy() || this.readOnly()) return false;
    const currentId = this.conversation()?.id;
    if (currentId && this.conversationWrites.has(currentId)) {
      this.error.set(
        new IpcError(
          'unavailable',
          'Changes to this conversation are still awaiting confirmation. Wait before saving the note again.'
        )
      );
      return false;
    }
    let cancel!: () => void;
    const cancelled = new Promise<never>((_resolve, reject) => {
      cancel = () => reject(new IpcError('cancelled', 'The note save is no longer current.'));
    });
    const operation = { cancel };
    this.noteSave = operation;
    this.savingNote.set(true);
    this.noteCreationId.set(null);
    const context = this.openRequest;
    const current = () => this.noteSave === operation && context === this.openRequest && !this.destroyRef.destroyed;
    const wait = <T>(request: () => Promise<T>, label: string) =>
      withRequestDeadline(() => Promise.race([request(), cancelled]), CHAT_NOTE_SAVE_TIMEOUT_MS, label, this.destroyRef);
    let noteDispatched = false;
    try {
      let conversation = this.conversation();
      if (!conversation) {
        conversation = await wait(
          () => this.api.createConversation(this.draftProjectId(), this.temporaryDraft()),
          'Creating the conversation for the note'
        );
        if (!current()) return false;
        this.noteCreationId.set(conversation.id);
        this.conversation.set(conversation);
      }
      const id = conversation.id;
      this.conversationWrites.add(id);
      let native: Promise<Conversation>;
      try {
        noteDispatched = true;
        native = this.api.updateConversation(id, {
          title: null,
          projectId: null,
          clearProject: false,
          pinnedNote: note,
        });
      } catch (error) {
        this.conversationWrites.delete(id);
        throw error;
      }
      // A frontend deadline does not cancel a native write. Keep its fence
      // until acknowledgement so a retry cannot overtake the old mutation.
      void native.then(
        () => this.conversationWrites.delete(id),
        () => this.conversationWrites.delete(id)
      );
      const updated = await wait(() => native, 'Saving the pinned note');
      if (!current()) return false;
      if (updated.id !== id) throw new IpcError('internal', 'The saved note does not belong to this conversation.');
      this.invalidateConversationList();
      if (this.conversation()?.id === id) this.conversation.set(updated);
      this.conversations.update((list) => list.map((item) => (item.id === id ? updated : item)));
      this.error.set(null);
      void this.loadConversations();
      return true;
    } catch (error) {
      if (current()) {
        const failure = IpcError.from(error);
        this.error.set(
          failure.code === 'timeout' && failure.origin === 'local'
            ? new IpcError(
                'timeout',
                `${failure.message} ${noteDispatched ? 'The save outcome is unknown.' : 'The conversation may have been created, but the note was not saved.'} No save was retried automatically.`
              )
            : failure
        );
      }
      return false;
    } finally {
      if (this.noteSave === operation) {
        this.noteSave = null;
        this.savingNote.set(false);
      }
    }
  }

  async remove(id: string): Promise<'closed' | 'deleted' | false> {
    if (this.destroyRef.destroyed || this.conversationWrites.has(id) || (this.busy() && this.conversation()?.id === id))
      return false;
    this.conversationWrites.add(id);
    this.deletingIds.update((ids) => [...ids, id]);
    if (this.conversation()?.id === id) {
      this.invalidateNoteSave();
      this.invalidateSavedMessages();
    }
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
          this.editorContext.update((editor) => editor + 1);
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
          errorCode: event.error.code,
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

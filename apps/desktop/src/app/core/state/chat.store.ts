import { Injectable, computed, inject, signal } from '@angular/core';
import type { ChatMessage, ChatStreamEvent } from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';
import { type ModelOption, ModelsStore, optionKey } from './models.store';

export type ChatEntryStatus = 'complete' | 'streaming' | 'cancelled' | 'failed';

export interface ChatEntry {
  readonly id: number;
  readonly role: 'user' | 'assistant';
  readonly content: string;
  readonly status: ChatEntryStatus;
  readonly model?: string;
  readonly error?: string;
  readonly elapsedMs?: number;
}

/** `connecting`: request sent, waiting for `started`; `streaming`: receiving text. */
export type ChatPhase = 'idle' | 'connecting' | 'streaming';

/**
 * Upper bound of messages sent as context. Real context management (token
 * budgets, retrieval, summarization) arrives with the Memory Engine.
 */
export const MAX_CONTEXT_MESSAGES = 40;
export const MAX_MESSAGE_LENGTH = 32_000;

@Injectable({ providedIn: 'root' })
export class ChatStore {
  private readonly api = inject(JarvisApi);
  private readonly models = inject(ModelsStore);
  private nextId = 1;
  private cancelRequested = false;
  /** `null` = automatic: the core's model router decides. */
  private readonly selectedKey = signal<string | null>(null);

  readonly entries = signal<readonly ChatEntry[]>([]);
  readonly phase = signal<ChatPhase>('idle');
  readonly activeRequestId = signal<string | null>(null);

  readonly busy = computed(() => this.phase() !== 'idle');
  readonly modelOptions = this.models.chatOptions;
  readonly modelsError = this.models.error;
  readonly automaticRoute = this.models.generalRoute;
  /** Explicitly selected model; `null` when automatic or no longer available. */
  readonly selected = computed<ModelOption | null>(() => {
    const key = this.selectedKey();
    return key === null
      ? null
      : (this.modelOptions().find((o) => optionKey(o.provider.id, o.model.id) === key) ?? null);
  });
  /** True when models are loaded and no real AI model is reachable. */
  readonly diagnosticOnly = computed(
    () => this.models.loaded() && !this.models.realModelAvailable(),
  );

  loadModels(force = false): Promise<void> {
    return this.models.refresh(force);
  }

  /** Selects a model, or automatic routing with `null`. */
  select(option: ModelOption | null): void {
    this.selectedKey.set(option ? optionKey(option.provider.id, option.model.id) : null);
  }

  async send(text: string): Promise<void> {
    const content = text.trim();
    if (!content || this.busy() || content.length > MAX_MESSAGE_LENGTH) {
      return;
    }

    const history = this.contextMessages();
    const userEntry = this.addEntry({ role: 'user', content, status: 'complete' });
    const assistantId = this.addEntry({ role: 'assistant', content: '', status: 'streaming' }).id;
    this.phase.set('connecting');
    this.cancelRequested = false;

    const selection = this.selected();
    try {
      const requestId = await this.api.startChat(
        {
          providerId: selection?.provider.id ?? null,
          model: selection?.model.id ?? null,
          messages: [...history, { role: 'user', content: userEntry.content }],
          temperature: null,
          maxOutputTokens: null,
        },
        (event) => this.onEvent(assistantId, event),
      );
      if (this.phase() !== 'idle' && this.activeRequestId() === null) {
        this.activeRequestId.set(requestId);
      }
      if (this.cancelRequested && this.phase() !== 'idle') {
        await this.cancel();
      }
    } catch (error) {
      const ipcError = IpcError.from(error);
      this.updateEntry(assistantId, { status: 'failed', error: ipcError.message });
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

  clear(): void {
    if (!this.busy()) {
      this.entries.set([]);
    }
  }

  private onEvent(entryId: number, event: ChatStreamEvent): void {
    switch (event.type) {
      case 'started':
        this.activeRequestId.set(event.requestId);
        this.phase.set('streaming');
        this.updateEntry(entryId, { model: event.model });
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
  }

  private contextMessages(): ChatMessage[] {
    return this.entries()
      .filter((e) => e.content.length > 0 && e.status !== 'failed')
      .slice(-(MAX_CONTEXT_MESSAGES - 1))
      .map((e) => ({ role: e.role, content: e.content }));
  }

  private addEntry(entry: Omit<ChatEntry, 'id'>): ChatEntry {
    const created: ChatEntry = { ...entry, id: this.nextId++ };
    this.entries.update((list) => [...list, created]);
    return created;
  }

  private updateEntry(id: number, patch: Partial<Omit<ChatEntry, 'id'>>): void {
    this.entries.update((list) => list.map((e) => (e.id === id ? { ...e, ...patch } : e)));
  }

  private appendContent(id: number, text: string): void {
    this.entries.update((list) =>
      list.map((e) => (e.id === id ? { ...e, content: e.content + text } : e)),
    );
  }
}

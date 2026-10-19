import { Injectable, computed, inject, signal } from '@angular/core';
import type {
  ChatMessage,
  ChatStreamEvent,
  ModelInfo,
  ProviderDescriptor,
  ProviderModels,
} from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';
import { SettingsStore } from './settings.store';

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

export interface ModelOption {
  readonly provider: ProviderDescriptor;
  readonly model: ModelInfo;
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
  private readonly settingsStore = inject(SettingsStore);
  private nextId = 1;
  private cancelRequested = false;

  readonly entries = signal<readonly ChatEntry[]>([]);
  readonly phase = signal<ChatPhase>('idle');
  readonly activeRequestId = signal<string | null>(null);
  readonly providers = signal<readonly ProviderModels[]>([]);
  readonly selected = signal<ModelOption | null>(null);
  readonly modelsError = signal<IpcError | null>(null);

  readonly busy = computed(() => this.phase() !== 'idle');
  readonly modelOptions = computed<ModelOption[]>(() =>
    this.providers().flatMap((p) =>
      p.models.filter((m) => m.capabilities.chat).map((model) => ({ provider: p.provider, model })),
    ),
  );
  /** True when no real AI provider is registered (Phase 1). */
  readonly diagnosticOnly = computed(() => {
    const providers = this.providers();
    return providers.length > 0 && providers.every((p) => p.provider.kind === 'diagnostic');
  });

  async loadModels(): Promise<void> {
    try {
      const providers = await this.api.listAiModels();
      this.providers.set(providers);
      this.modelsError.set(null);
      this.ensureSelection();
    } catch (error) {
      this.modelsError.set(IpcError.from(error));
    }
  }

  select(option: ModelOption): void {
    this.selected.set(option);
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

  private ensureSelection(): void {
    const options = this.modelOptions();
    const current = this.selected();
    if (
      current &&
      options.some((o) => o.provider.id === current.provider.id && o.model.id === current.model.id)
    ) {
      return;
    }
    const ai = this.settingsStore.settings()?.ai;
    const preferred =
      options.find(
        (o) =>
          (!ai?.defaultProviderId || o.provider.id === ai.defaultProviderId) &&
          o.model.id === ai?.defaultModel,
      ) ??
      options.find((o) => o.provider.kind !== 'diagnostic') ??
      options[0] ??
      null;
    this.selected.set(preferred);
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

import { TestBed } from '@angular/core/testing';
import type {
  CaptureInfo,
  ChatStreamEvent,
  ChatTurnRequest,
  ContextReport,
  Conversation,
  ConversationDetail,
  ConversationPatch,
  ProviderModels,
  RouteResolution,
} from '@jarvis/shared-types';

import { SETTINGS } from '../../testing/settings.fixture';
import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';
import {
  ChatStore,
  CHAT_CANCEL_TIMEOUT_MS,
  CHAT_NOTE_SAVE_TIMEOUT_MS,
  CHAT_SEND_TIMEOUT_MS,
  CONVERSATION_LIST_TIMEOUT_MS,
  CONVERSATION_OPEN_TIMEOUT_MS,
} from './chat.store';
import { SettingsStore } from './settings.store';
import { VisionStore } from './vision.store';

const ECHO: ProviderModels = {
  provider: {
    id: 'diagnostic-echo',
    displayName: 'Diagnostic echo',
    kind: 'diagnostic',
    locality: 'onDevice',
  },
  models: [
    {
      id: 'echo',
      displayName: 'Echo (diagnostic)',
      providerId: 'diagnostic-echo',
      contextWindow: null,
      capabilities: {
        chat: true,
        streaming: true,
        embeddings: false,
        toolCalling: false,
        vision: false,
      },
    },
  ],
  error: null,
};

function conversation(id: string, patch: Partial<Conversation> = {}): Conversation {
  return {
    id,
    title: 'New conversation',
    projectId: null,
    pinnedNote: '',
    temporary: false,
    messageCount: 0,
    createdAtMs: 1,
    updatedAtMs: 1,
    ...patch,
  };
}

const CONTEXT: ContextReport = {
  windowTokens: 4096,
  budgetTokens: 2764,
  usedTokens: 120,
  systemTokens: 80,
  projectTokens: 0,
  noteTokens: 0,
  memoryTokens: 20,
  summaryTokens: 0,
  historyTokens: 0,
  historyIncluded: 0,
  historyOmitted: 0,
  summarizedMessages: 0,
  factsIncluded: 1,
  recallsIncluded: 0,
  project: null,
  needsCompaction: false,
  memorySkipped: null,
};

const CAPTURE: CaptureInfo = {
  id: 'new-draft-image',
  source: 'screen',
  label: 'Display 1',
  width: 100,
  height: 100,
  preview: 'data:image/jpeg;base64,AAAA',
  capturedAtMs: 1,
};

class FakeApi {
  connected = true;
  turns: ChatTurnRequest[] = [];
  created: { projectId: string | null; temporary: boolean }[] = [];
  patches: { id: string; patch: ConversationPatch }[] = [];
  deleted: string[] = [];
  cancelled: string[] = [];
  list: Conversation[] = [];
  emit: (event: ChatStreamEvent) => void = () => undefined;
  sendError: IpcError | null = null;

  listAiModels = vi.fn(async () => [ECHO]);
  getModelRoutes = vi.fn(async (): Promise<RouteResolution[]> => [
    {
      category: 'general',
      providerId: 'diagnostic-echo',
      model: 'echo',
      automatic: true,
      error: null,
    },
  ]);
  getSettings = vi.fn();
  captureScreen = vi.fn(async (): Promise<CaptureInfo> => CAPTURE);
  discardCapture = vi.fn(async () => true);
  listConversations = vi.fn(async () => this.list);

  async getConversation(id: string): Promise<ConversationDetail> {
    return {
      conversation: conversation(id, { title: 'Saved chat', messageCount: 2 }),
      messages: [
        {
          id: 'm1',
          role: 'user',
          content: 'hello',
          status: 'complete',
          providerId: null,
          model: null,
          tokens: 2,
          createdAtMs: 1,
        },
        {
          id: 'm2',
          role: 'assistant',
          content: 'partial',
          status: 'cancelled',
          providerId: 'diagnostic-echo',
          model: 'echo',
          tokens: 2,
          createdAtMs: 2,
        },
      ],
      summary: null,
    };
  }

  async createConversation(projectId: string | null, temporary: boolean): Promise<Conversation> {
    this.created.push({ projectId, temporary });
    const created = conversation(`c${this.created.length}`, { projectId, temporary });
    this.list = [created, ...this.list];
    return created;
  }

  async updateConversation(id: string, patch: ConversationPatch): Promise<Conversation> {
    this.patches.push({ id, patch });
    return conversation(id, {
      title: patch.title ?? 'New conversation',
      projectId: patch.clearProject ? null : patch.projectId,
      pinnedNote: patch.pinnedNote ?? '',
    });
  }

  async deleteConversation(id: string): Promise<void> {
    this.deleted.push(id);
  }

  async sendMessage(request: ChatTurnRequest, onEvent: (e: ChatStreamEvent) => void): Promise<string> {
    if (this.sendError) {
      throw this.sendError;
    }
    this.turns.push(request);
    this.emit = onEvent;
    return 'req-1';
  }

  async cancelChat(requestId: string): Promise<boolean> {
    this.cancelled.push(requestId);
    return true;
  }
}

function started(): ChatStreamEvent {
  return {
    type: 'started',
    requestId: 'req-1',
    providerId: 'diagnostic-echo',
    model: 'echo',
    conversationId: 'c1',
    messageId: 'reply-1',
    context: CONTEXT,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

describe('ChatStore', () => {
  let store: ChatStore;
  let api: FakeApi;

  beforeEach(async () => {
    api = new FakeApi();
    TestBed.configureTestingModule({ providers: [{ provide: JarvisApi, useValue: api }] });
    TestBed.inject(SettingsStore).settings.set(SETTINGS);
    store = TestBed.inject(ChatStore);
    await store.loadModels();
  });

  it('defaults to automatic routing and flags diagnostic-only mode', () => {
    expect(store.selected()).toBeNull();
    expect(store.selection()).toBeNull();
    expect(store.selectionUnavailable()).toBe(false);
    expect(store.automaticRoute()?.model).toBe('echo');
    expect(store.diagnosticOnly()).toBe(true);
  });

  it('refuses a vanished explicit model without creating a conversation or adding messages', async () => {
    store.select(store.modelOptions()[0]);
    api.listAiModels.mockResolvedValueOnce([]);
    await store.loadModels(true);
    expect(store.selection()).toEqual({ providerId: 'diagnostic-echo', model: 'echo' });
    expect(store.selected()).toBeNull();
    expect(store.selectionUnavailable()).toBe(true);
    expect(store.acceptsInput()).toBe(false);

    await store.send('Do not reroute this message');

    expect(api.created).toEqual([]);
    expect(api.turns).toEqual([]);
    expect(store.entries()).toEqual([]);
    expect(store.conversation()).toBeNull();
    expect(store.phase()).toBe('idle');
    expect(store.error()).toMatchObject({ code: 'unavailable' });
    expect(store.error()?.message).toContain('select Automatic');
  });

  it('uses the retained explicit choice when the same model becomes available again', async () => {
    store.select(store.modelOptions()[0]);
    api.listAiModels.mockResolvedValueOnce([]);
    await store.loadModels(true);
    await store.send('Unavailable');
    api.listAiModels.mockResolvedValueOnce([ECHO]);
    await store.loadModels(true);

    expect(store.selectionUnavailable()).toBe(false);
    expect(store.acceptsInput()).toBe(true);
    await store.send('Available again');
    expect(api.turns).toHaveLength(1);
    expect(api.turns[0]).toMatchObject({ providerId: 'diagnostic-echo', model: 'echo' });
    expect(store.error()).toBeNull();
  });

  it('uses automatic routing only after the user explicitly selects it', async () => {
    store.select(store.modelOptions()[0]);
    api.listAiModels.mockResolvedValueOnce([]);
    await store.loadModels(true);
    await store.send('Unavailable');

    store.select(null);

    expect(store.selection()).toBeNull();
    expect(store.selectionUnavailable()).toBe(false);
    expect(store.error()).toBeNull();
    await store.send('Use automatic routing');
    expect(api.turns).toHaveLength(1);
    expect(api.turns[0]).toMatchObject({ providerId: null, model: null });
  });

  it.each(['disappears', 'changes selection'] as const)(
    'keeps the original explicit model if discovery %s while the conversation is being created',
    async (change) => {
      store.select(store.modelOptions()[0]);
      const creation = deferred<Conversation>();
      vi.spyOn(api, 'createConversation').mockReturnValueOnce(creation.promise);
      const sending = store.send('Keep this model');
      if (change === 'disappears') {
        api.listAiModels.mockResolvedValueOnce([]);
        await store.loadModels(true);
      } else {
        store.select(null);
      }
      creation.resolve(conversation('deferred'));
      await sending;

      expect(api.turns).toHaveLength(1);
      expect(api.turns[0]).toMatchObject({
        conversationId: 'deferred',
        providerId: 'diagnostic-echo',
        model: 'echo',
      });
    }
  );

  it('does not replace a busy turn or its existing error when its selected model vanishes', async () => {
    store.select(store.modelOptions()[0]);
    await store.send('First turn');
    const existingError = new IpcError('io', 'An unrelated conversation operation failed');
    store.error.set(existingError);
    api.listAiModels.mockResolvedValueOnce([]);
    await store.loadModels(true);

    await store.send('Must not replace the running turn');

    expect(api.turns).toHaveLength(1);
    expect(store.entries()).toHaveLength(2);
    expect(store.error()).toBe(existingError);
    expect(store.phase()).toBe('connecting');
  });

  it.each(['resolve', 'reject'])('ignores an obsolete search %s after the newer search finishes', async (outcome) => {
    const old = deferred<Conversation[]>();
    api.listConversations.mockReturnValueOnce(old.promise);
    store.setConversationQuery('old');
    const pending = store.loadConversations();
    store.setConversationQuery('new');
    api.listConversations.mockResolvedValueOnce([conversation('new')]);
    await store.loadConversations();
    if (outcome === 'resolve') old.resolve([conversation('old')]);
    else old.reject(new Error('obsolete failure'));
    await pending;
    expect(store.conversations().map((c) => c.id)).toEqual(['new']);
    expect(store.conversationsError()).toBeNull();
    expect(store.conversationsLoading()).toBe(false);
    expect(api.listConversations).toHaveBeenLastCalledWith('new');
  });

  it('invalidates a search immediately when typing away and back before debounce', async () => {
    const old = deferred<Conversation[]>();
    api.listConversations.mockReturnValueOnce(old.promise);
    store.setConversationQuery('a');
    const pending = store.loadConversations();
    store.setConversationQuery('b');
    store.setConversationQuery('a');
    old.resolve([conversation('obsolete')]);
    await pending;
    expect(store.conversations()).toEqual([]);
    expect(store.conversationsError()).toBeNull();
  });

  it('keeps the latest loading state when an older search finishes', async () => {
    const old = deferred<Conversation[]>();
    const fresh = deferred<Conversation[]>();
    api.listConversations.mockReturnValueOnce(old.promise).mockReturnValueOnce(fresh.promise);
    const first = store.loadConversations();
    const second = store.loadConversations();
    old.resolve([]);
    await first;
    expect(store.conversationsLoading()).toBe(true);
    fresh.resolve([conversation('new')]);
    await second;
    expect(store.conversationsLoading()).toBe(false);
  });

  it('keeps search errors separate from chat errors and preserves rows on failed refresh', async () => {
    store.conversations.set([conversation('saved')]);
    store.error.set(new IpcError('unavailable', 'Chat error'));
    api.listConversations.mockRejectedValueOnce(new Error('Search unavailable'));
    await store.loadConversations();
    expect(store.conversations()).toHaveLength(1);
    expect(store.conversationsError()?.message).toContain('Search unavailable');
    expect(store.error()?.message).toBe('Chat error');
    await store.loadConversations();
    expect(store.conversationsError()).toBeNull();
    expect(store.error()?.message).toBe('Chat error');
  });

  it('times out a search and ignores its late response after retry', async () => {
    vi.useFakeTimers();
    try {
      const old = deferred<Conversation[]>();
      api.listConversations.mockReturnValueOnce(old.promise);
      const pending = store.loadConversations();
      await vi.advanceTimersByTimeAsync(CONVERSATION_LIST_TIMEOUT_MS);
      await pending;
      expect(store.conversationsLoading()).toBe(false);
      expect(store.conversationsError()?.code).toBe('timeout');
      api.listConversations.mockResolvedValueOnce([conversation('new')]);
      await store.loadConversations();
      old.resolve([conversation('old')]);
      await Promise.resolve();
      expect(store.conversations().map((c) => c.id)).toEqual(['new']);
      expect(store.conversationsError()).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it.each(['rename', 'delete'])('does not restore stale list rows after %s', async (action) => {
    store.conversations.set([conversation('c1')]);
    const old = deferred<Conversation[]>();
    api.listConversations.mockReturnValueOnce(old.promise);
    const pending = store.loadConversations();
    if (action === 'rename') await store.rename('c1', 'Renamed');
    else await store.remove('c1');
    old.resolve([conversation('c1')]);
    await pending;
    if (action === 'rename') expect(store.conversations()[0].title).toBe('Renamed');
    else expect(store.conversations()).toEqual([]);
  });

  it('settles search on destruction and prevents later requests or updates', async () => {
    const old = deferred<Conversation[]>();
    api.listConversations.mockReturnValueOnce(old.promise);
    const pending = store.loadConversations();
    TestBed.resetTestingModule();
    await pending;
    old.resolve([conversation('late')]);
    await store.loadConversations();
    expect(store.conversations()).toEqual([]);
    expect(store.conversationsLoading()).toBe(false);
    expect(store.conversationsError()).toBeNull();
    expect(api.listConversations).toHaveBeenCalledTimes(1);
  });

  it('creates the conversation with the first message and sends only that message', async () => {
    store.select(store.modelOptions()[0]);
    await store.send('  hello  ');
    expect(api.created).toEqual([{ projectId: null, temporary: false }]);
    expect(api.turns[0]).toEqual({
      conversationId: 'c1',
      content: 'hello',
      providerId: 'diagnostic-echo',
      model: 'echo',
      temperature: null,
      maxOutputTokens: null,
      speak: false,
      attachments: [],
      mentor: null,
    });
    expect(store.conversation()?.id).toBe('c1');
    expect(store.phase()).toBe('connecting');
  });

  it('sends attached images by id and shows them with the message', async () => {
    const capture = {
      id: 'shot-1',
      source: 'screen' as const,
      label: 'Display 1',
      width: 1568,
      height: 882,
      preview: 'data:image/jpeg;base64,AAAA',
      capturedAtMs: 1,
    };
    await store.send('Що на екрані?', true, [capture]);
    expect(api.turns[0].attachments).toEqual(['shot-1']);
    expect(api.turns[0].speak).toBe(true);
    expect(store.entries()[0].images).toEqual([capture]);
  });

  it('reports preflight and conversation creation failures as not sent', async () => {
    const onResult = vi.fn();
    expect(await store.send(' ', false, [], onResult)).toBe('notSent');
    expect(onResult).toHaveBeenCalledExactlyOnceWith('notSent');
    onResult.mockClear();
    vi.spyOn(api, 'createConversation').mockRejectedValueOnce(new Error('Storage is unavailable'));
    expect(await store.send('Keep this draft', false, [], onResult)).toBe('notSent');
    expect(onResult).toHaveBeenCalledExactlyOnceWith('notSent');
    expect(api.turns).toEqual([]);
    expect(store.busy()).toBe(false);
  });

  it('reports a core admission refusal without pretending the turn started', async () => {
    const onResult = vi.fn();
    api.sendError = new IpcError('permissionDenied', 'Blocked by policy', 'core');
    expect(await store.send('Keep this draft', false, [], onResult)).toBe('notSent');
    expect(onResult).toHaveBeenCalledExactlyOnceWith('notSent');
    expect(store.entries()[1]).toMatchObject({ status: 'failed', error: 'Blocked by policy' });
    expect(store.busy()).toBe(false);
  });

  it('reports a bridge refusal before native invocation as not sent', async () => {
    const onResult = vi.fn();
    api.sendError = new IpcError('unavailable', 'The native bridge is unavailable', 'notDispatched');
    expect(await store.send('Keep this draft', false, [], onResult)).toBe('notSent');
    expect(onResult).toHaveBeenCalledExactlyOnceWith('notSent');
    expect(store.entries()[1]).toMatchObject({ status: 'failed', error: 'The native bridge is unavailable' });
    expect(store.busy()).toBe(false);
  });

  it.each([
    new IpcError('io', 'Storage failed', 'core'),
    new IpcError('internal', 'Core task failed', 'core'),
    new IpcError('permissionDenied', 'Transport failed', 'transport'),
    new IpcError('timeout', 'Local deadline expired'),
  ])('keeps the outcome unknown for an ambiguous dispatched error: %s', async (error) => {
    const onResult = vi.fn();
    api.sendError = error;
    expect(await store.send('Do not resend', false, [], onResult)).toBe('unknown');
    expect(onResult).toHaveBeenCalledExactlyOnceWith('unknown');
    expect(store.entries()[1].error).toContain('may still be running');
    expect(store.entries()[1].error).toContain('No message was resent');
    expect(store.entries()[1].error).toContain('Check saved messages');
    expect(store.busy()).toBe(false);
  });

  it.each(['resolve', 'reject', 'timeout'] as const)(
    'reports admission immediately and keeps streaming after a late invoke %s',
    async (outcome) => {
      await store.open('c1');
      const response = deferred<string>();
      let emit!: (event: ChatStreamEvent) => void;
      vi.spyOn(api, 'sendMessage').mockImplementationOnce((_request, onEvent) => {
        emit = onEvent;
        return response.promise;
      });
      const onResult = vi.fn();
      if (outcome === 'timeout') vi.useFakeTimers();
      const sending = store.send('Accepted turn', false, [], onResult);
      expect(emit).toBeDefined();
      emit(started());
      expect(onResult).toHaveBeenCalledExactlyOnceWith('accepted');
      expect(store.busy()).toBe(true);
      if (outcome === 'resolve') response.resolve('req-1');
      else if (outcome === 'reject') response.reject(new IpcError('privacyBlocked', 'Late failure', 'core'));
      else await vi.advanceTimersByTimeAsync(CHAT_SEND_TIMEOUT_MS);
      try {
        expect(await sending).toBe('accepted');
      } finally {
        vi.useRealTimers();
      }
      expect(onResult).toHaveBeenCalledOnce();
      expect(store.phase()).toBe('streaming');
      expect(store.entries().at(-1)).toMatchObject({ status: 'streaming' });
      expect(store.entries().at(-1)?.error).toBeUndefined();
      emit({ type: 'finished', finishReason: 'stop', usage: null, elapsedMs: 1 });
      expect(store.busy()).toBe(false);
    }
  );

  it('reports an unacknowledged dispatch as unknown at its deadline and ignores late events', async () => {
    await store.open('c1');
    const response = deferred<string>();
    let emit!: (event: ChatStreamEvent) => void;
    vi.spyOn(api, 'sendMessage').mockImplementationOnce((_request, onEvent) => {
      emit = onEvent;
      return response.promise;
    });
    const onResult = vi.fn();
    vi.useFakeTimers();
    try {
      const sending = store.send('Uncertain turn', false, [], onResult);
      await vi.advanceTimersByTimeAsync(CHAT_SEND_TIMEOUT_MS);
      expect(await sending).toBe('unknown');
      expect(onResult).toHaveBeenCalledExactlyOnceWith('unknown');
      const entries = store.entries();
      response.resolve('late-id');
      emit(started());
      emit({ type: 'delta', text: 'late reply' });
      await Promise.resolve();
      expect(store.entries()).toBe(entries);
      expect(store.busy()).toBe(false);
      expect(onResult).toHaveBeenCalledOnce();
    } finally {
      vi.useRealTimers();
    }
  });

  it('times out conversation creation without dispatching a late result', async () => {
    const response = deferred<Conversation>();
    vi.spyOn(api, 'createConversation').mockReturnValueOnce(response.promise);
    const onResult = vi.fn();
    vi.useFakeTimers();
    try {
      const sending = store.send('Keep this draft', false, [], onResult);
      await vi.advanceTimersByTimeAsync(CHAT_SEND_TIMEOUT_MS);
      expect(await sending).toBe('notSent');
      expect(onResult).toHaveBeenCalledExactlyOnceWith('notSent');
      response.resolve(conversation('late'));
      await Promise.resolve();
      expect(api.turns).toEqual([]);
      expect(store.conversation()).toBeNull();
      expect(store.busy()).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });

  it('streams the reply with its context report and refreshes the title', async () => {
    await store.send('hello');
    api.list = [conversation('c1', { title: 'hello', messageCount: 2 })];
    api.emit(started());
    expect(store.phase()).toBe('streaming');
    api.emit({ type: 'delta', text: '[echo] ' });
    api.emit({ type: 'delta', text: 'hello' });
    api.emit({ type: 'finished', finishReason: 'stop', usage: null, elapsedMs: 12 });

    const [user, assistant] = store.entries();
    expect(user).toMatchObject({ role: 'user', content: 'hello' });
    expect(assistant).toMatchObject({
      role: 'assistant',
      content: '[echo] hello',
      status: 'complete',
      model: 'echo',
      elapsedMs: 12,
      context: CONTEXT,
    });
    expect(store.phase()).toBe('idle');
    await vi.waitFor(() => expect(store.conversation()?.title).toBe('hello'));
  });

  it('reuses the open conversation for later messages', async () => {
    await store.send('first');
    api.emit({ type: 'finished', finishReason: 'stop', usage: null, elapsedMs: 1 });
    await store.send('second');
    expect(api.created).toHaveLength(1);
    expect(api.turns.map((t) => t.conversationId)).toEqual(['c1', 'c1']);
  });

  it('starts temporary chats and chats in a project', async () => {
    store.startNew(true);
    expect(store.temporary()).toBe(true);
    await store.send('secret');
    expect(api.created[0]).toEqual({ projectId: null, temporary: true });

    api.emit({ type: 'finished', finishReason: 'stop', usage: null, elapsedMs: 1 });
    store.startNew(false, 'p1');
    expect(store.projectId()).toBe('p1');
    await store.send('in project');
    expect(api.created[1]).toEqual({ projectId: 'p1', temporary: false });
  });

  it('opens saved conversations', async () => {
    await store.open('c9');
    expect(store.conversation()?.title).toBe('Saved chat');
    expect(store.entries().map((e) => [e.role, e.status])).toEqual([
      ['user', 'complete'],
      ['assistant', 'cancelled'],
    ]);
  });

  it.each([false, true])('waits for an opened conversation before sending (existing chat: %s)', async (existing) => {
    if (existing) await store.open('current');
    const previous = store.conversation();
    const messages = store.entries();
    const detail = await api.getConversation('next');
    const response = deferred<ConversationDetail>();
    vi.spyOn(api, 'getConversation').mockReturnValueOnce(response.promise);
    const opening = store.open('next');

    await store.send('For the next conversation');
    expect(api.created).toEqual([]);
    expect(api.turns).toEqual([]);
    expect(store.conversation()).toBe(previous);
    expect(store.entries()).toBe(messages);
    expect(store.phase()).toBe('idle');
    expect(store.loading()).toBe(true);
    expect(store.acceptsInput()).toBe(false);

    response.resolve(detail);
    await opening;
    expect(store.conversation()?.id).toBe('next');
    expect(store.loading()).toBe(false);
    await store.send('For the next conversation');
    expect(api.turns).toHaveLength(1);
    expect(api.turns[0].conversationId).toBe('next');
    expect(api.created).toEqual([]);
  });

  it('times out opening a conversation, preserves the current chat and permits retry', async () => {
    await store.open('current');
    const detail = await api.getConversation('next');
    const late = deferred<ConversationDetail>();
    vi.spyOn(api, 'getConversation').mockReturnValueOnce(late.promise);
    vi.useFakeTimers();
    try {
      const pending = store.open('next');
      await vi.advanceTimersByTimeAsync(CONVERSATION_OPEN_TIMEOUT_MS);
      await pending;
      expect(store.loading()).toBe(false);
      expect(store.error()?.code).toBe('timeout');
      expect(store.conversation()?.id).toBe('current');
      await store.open('retry');
      late.resolve(detail);
      await Promise.resolve();
      expect(store.conversation()?.id).toBe('retry');
      expect(store.error()).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it('keeps the newer open loading state when an older open times out', async () => {
    const next = deferred<ConversationDetail>();
    const detail = await api.getConversation('next');
    vi.spyOn(api, 'getConversation')
      .mockReturnValueOnce(new Promise(() => undefined))
      .mockReturnValueOnce(next.promise);
    vi.useFakeTimers();
    try {
      const first = store.open('old');
      await vi.advanceTimersByTimeAsync(1000);
      const second = store.open('next');
      await vi.advanceTimersByTimeAsync(CONVERSATION_OPEN_TIMEOUT_MS - 1000);
      await first;
      expect(store.loading()).toBe(true);
      expect(store.error()).toBeNull();
      next.resolve(detail);
      await second;
      expect(store.loading()).toBe(false);
      expect(store.conversation()?.id).toBe('next');
    } finally {
      vi.useRealTimers();
    }
  });

  it('settles a pending conversation open on destruction without applying late data', async () => {
    const detail = await api.getConversation('late');
    const late = deferred<ConversationDetail>();
    const get = vi.spyOn(api, 'getConversation').mockReturnValueOnce(late.promise);
    const pending = store.open('late');
    TestBed.resetTestingModule();
    await pending;
    expect(store.loading()).toBe(false);
    late.resolve(detail);
    await store.open('new');
    expect(get).toHaveBeenCalledTimes(1);
    expect(store.conversation()).toBeNull();
    expect(store.entries()).toEqual([]);
    expect(store.error()).toBeNull();
  });

  it('makes saved conversations read-only while saving is off', async () => {
    await store.open('c9');
    TestBed.inject(SettingsStore).settings.set({
      ...SETTINGS,
      memory: { ...SETTINGS.memory, enabled: false },
    });
    expect(store.readOnly()).toBe(true);
    await store.send('more');
    expect(api.turns).toHaveLength(0);

    store.startNew();
    expect(store.temporary()).toBe(true);
  });

  it('moves conversations between projects and edits the pinned note', async () => {
    await store.setProject('p1');
    expect(store.projectId()).toBe('p1');
    expect(api.patches).toHaveLength(0);

    expect(await store.setPinnedNote('Goal: ship it')).toBe(true);
    expect(api.created).toEqual([{ projectId: 'p1', temporary: false }]);
    expect(store.conversation()?.pinnedNote).toBe('Goal: ship it');

    await store.setProject(null);
    expect(api.patches.at(-1)?.patch).toMatchObject({ clearProject: true, projectId: null });
  });

  it.each([false, true])('refuses note and project edits during a pending open (existing chat: %s)', async (existing) => {
    if (existing) await store.open('current');
    const previous = store.conversation();
    const project = store.projectId();
    const detail = await api.getConversation('next');
    const response = deferred<ConversationDetail>();
    vi.spyOn(api, 'getConversation').mockReturnValueOnce(response.promise);
    const opening = store.open('next');

    expect(await store.setPinnedNote('Must not be attached to the old chat')).toBe(false);
    await store.setProject('next-project');
    expect(api.created).toEqual([]);
    expect(api.patches).toEqual([]);
    expect(store.conversation()).toBe(previous);
    expect(store.projectId()).toBe(project);
    expect(store.loading()).toBe(true);
    expect(store.savingNote()).toBe(false);

    response.resolve(detail);
    await opening;
    expect(await store.setPinnedNote('Note for the new chat')).toBe(true);
    await store.setProject('next-project');
    expect(api.patches.map((patch) => patch.id)).toEqual(['next', 'next']);
    expect(store.projectId()).toBe('next-project');
    expect(api.created).toEqual([]);
  });

  it('does not attach a note-created conversation after switching chats', async () => {
    const created = deferred<Conversation>();
    vi.spyOn(api, 'createConversation').mockReturnValueOnce(created.promise);
    const saving = store.setPinnedNote('original note');
    await store.open('other');
    created.resolve(conversation('created'));
    expect(await saving).toBe(false);
    expect(store.conversation()?.id).toBe('other');
    expect(api.patches).toEqual([]);
    expect(store.savingNote()).toBe(false);
  });

  it('prevents duplicate note creation and sending during a note save', async () => {
    const created = deferred<Conversation>();
    const create = vi.spyOn(api, 'createConversation').mockReturnValueOnce(created.promise);
    const saving = store.setPinnedNote('note');
    expect(await store.setPinnedNote('duplicate')).toBe(false);
    await store.send('message');
    expect(create).toHaveBeenCalledTimes(1);
    expect(api.turns).toEqual([]);
    created.resolve(conversation('created'));
    expect(await saving).toBe(true);
    expect(api.patches).toHaveLength(1);
  });

  it('keeps the selected project fixed while creating a conversation for a pinned note', async () => {
    await store.setProject('original-project');
    const created = deferred<Conversation>();
    const create = vi.spyOn(api, 'createConversation').mockReturnValueOnce(created.promise);
    const updated = conversation('created', { projectId: 'original-project', pinnedNote: 'Note' });
    const update = vi.spyOn(api, 'updateConversation').mockResolvedValueOnce(updated);
    const saving = store.setPinnedNote('Note');
    await store.setProject('late-project');
    expect(store.projectId()).toBe('original-project');
    expect(create).toHaveBeenCalledExactlyOnceWith('original-project', false);
    expect(update).not.toHaveBeenCalled();
    created.resolve(conversation('created', { projectId: 'original-project' }));
    expect(await saving).toBe(true);
    expect(store.projectId()).toBe('original-project');
    expect(update).toHaveBeenCalledOnce();
  });

  it.each([false, true])('refuses project changes during a reply (existing conversation: %s)', async (existing) => {
    if (existing) await store.open('current');
    else await store.setProject('draft-project');
    const previous = store.projectId();
    store.phase.set('connecting');
    await store.setProject('other-project');
    expect(store.projectId()).toBe(previous);
    expect(api.patches).toEqual([]);
    expect(api.created).toEqual([]);
  });

  it.each(['resolve', 'reject'])('ignores stale metadata %s for a reopened conversation', async (outcome) => {
    await store.open('c1');
    const pending = deferred<Conversation>();
    vi.spyOn(api, 'updateConversation').mockReturnValueOnce(pending.promise);
    const saving = store.rename('c1', 'Old rename');
    await store.open('other');
    await store.open('c1');
    store.error.set(new IpcError('unavailable', 'Current error'));
    if (outcome === 'resolve') pending.resolve(conversation('c1', { title: 'Old rename' }));
    else pending.reject(new Error('Old failure'));
    await saving;
    expect(store.conversation()?.title).toBe('Saved chat');
    expect(store.error()?.message).toBe('Current error');
  });

  it('allows only one metadata write per conversation and unlocks after failure', async () => {
    await store.open('c1');
    const pending = deferred<Conversation>();
    const update = vi.spyOn(api, 'updateConversation').mockReturnValueOnce(pending.promise);
    const renaming = store.rename('c1', 'title');
    expect(await store.setPinnedNote('note')).toBe(false);
    expect(update).toHaveBeenCalledTimes(1);
    pending.reject(new Error('Write failed'));
    await renaming;
    expect(await store.setPinnedNote('retry')).toBe(true);
    expect(store.conversation()?.pinnedNote).toBe('retry');
  });

  it('does not publish a note save after destruction', async () => {
    await store.open('c1');
    const pending = deferred<Conversation>();
    vi.spyOn(api, 'updateConversation').mockReturnValueOnce(pending.promise);
    const saving = store.setPinnedNote('note');
    TestBed.resetTestingModule();
    pending.resolve(conversation('c1', { pinnedNote: 'late' }));
    expect(await saving).toBe(false);
    expect(store.conversation()?.pinnedNote).toBe('');
    expect(store.savingNote()).toBe(false);
    expect(await store.rename('c1', 'later')).toBe(false);
  });

  it('times out note conversation creation without applying or writing a late conversation', async () => {
    const created = deferred<Conversation>();
    const create = vi.spyOn(api, 'createConversation').mockReturnValueOnce(created.promise);
    vi.useFakeTimers();
    try {
      const saving = store.setPinnedNote('Keep my draft');
      await vi.advanceTimersByTimeAsync(CHAT_NOTE_SAVE_TIMEOUT_MS);
      expect(await saving).toBe(false);
      expect(store.savingNote()).toBe(false);
      expect(store.error()?.code).toBe('timeout');
      expect(store.error()?.message).toContain('conversation may have been created');
      expect(store.error()?.message).toContain('note was not saved');
      expect(store.conversation()).toBeNull();
      expect(store.noteCreationId()).toBeNull();
      expect(api.patches).toEqual([]);
      expect(create).toHaveBeenCalledOnce();
      created.resolve(conversation('late-created'));
      await Promise.resolve();
      expect(store.conversation()).toBeNull();
      expect(api.patches).toEqual([]);
      expect(await store.setPinnedNote('Explicit retry')).toBe(true);
      expect(store.conversation()?.pinnedNote).toBe('Explicit retry');
    } finally {
      vi.useRealTimers();
    }
  });

  it.each(['resolve', 'reject'] as const)(
    'retains a timed-out native note write fence until its late %s and ignores the stale result',
    async (outcome) => {
      await store.open('c1');
      const native = deferred<Conversation>();
      const update = vi.spyOn(api, 'updateConversation').mockReturnValueOnce(native.promise);
      vi.useFakeTimers();
      try {
        const saving = store.setPinnedNote('Timed-out note');
        await vi.advanceTimersByTimeAsync(CHAT_NOTE_SAVE_TIMEOUT_MS);
        expect(await saving).toBe(false);
        expect(store.savingNote()).toBe(false);
        expect(store.acceptsInput()).toBe(true);
        expect(store.error()?.code).toBe('timeout');
        expect(store.error()?.message).toContain('save outcome is unknown');
        expect(await store.setPinnedNote('Must wait for the old write')).toBe(false);
        expect(store.error()?.message).toContain('awaiting confirmation');
        expect(update).toHaveBeenCalledOnce();
        expect(await store.rename('c1', 'Must not race')).toBe(false);
        expect(await store.remove('c1')).toBe(false);
        if (outcome === 'resolve') native.resolve(conversation('c1', { pinnedNote: 'Timed-out note' }));
        else native.reject(new Error('Old native failure'));
        await Promise.resolve();
        await Promise.resolve();
        expect(store.conversation()?.pinnedNote).toBe('');
        expect(store.error()?.message).toContain('awaiting confirmation');
        expect(await store.setPinnedNote('Explicit latest note')).toBe(true);
        expect(store.conversation()?.pinnedNote).toBe('Explicit latest note');
        expect(update).toHaveBeenCalledTimes(2);
      } finally {
        vi.useRealTimers();
      }
    }
  );

  it('does not let an invalidated creation release the saving state of a newer note operation', async () => {
    const first = deferred<Conversation>();
    const second = deferred<Conversation>();
    const create = vi.spyOn(api, 'createConversation').mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    const old = store.setPinnedNote('Old draft');
    store.startNew();
    expect(await old).toBe(false);
    const fresh = store.setPinnedNote('New draft');
    first.resolve(conversation('stale-created'));
    await Promise.resolve();
    expect(store.savingNote()).toBe(true);
    expect(store.conversation()).toBeNull();
    expect(api.patches).toEqual([]);
    second.resolve(conversation('newly-created'));
    expect(await fresh).toBe(true);
    expect(store.savingNote()).toBe(false);
    expect(store.conversation()?.id).toBe('newly-created');
    expect(store.conversation()?.pinnedNote).toBe('New draft');
    expect(create).toHaveBeenCalledTimes(2);
    expect(api.patches.map((item) => item.id)).toEqual(['newly-created']);
  });

  it('confirms a successful note without waiting for a stalled conversation-list refresh', async () => {
    await store.open('c1');
    api.listConversations.mockReturnValueOnce(new Promise(() => undefined));
    expect(await store.setPinnedNote('Saved independently of list refresh')).toBe(true);
    expect(store.savingNote()).toBe(false);
    expect(store.conversation()?.pinnedNote).toBe('Saved independently of list refresh');
    expect(store.conversationsLoading()).toBe(true);
  });

  it.each(['io', 'timeout'] as const)(
    'reports a native note write %s exactly and releases its reservation for an explicit retry',
    async (code) => {
      await store.open('c1');
      const update = vi.spyOn(api, 'updateConversation').mockRejectedValueOnce({ code, message: 'Storage is unavailable' });
      expect(await store.setPinnedNote('Not confirmed')).toBe(false);
      expect(store.error()).toMatchObject({ code, message: 'Storage is unavailable' });
      expect(store.savingNote()).toBe(false);
      expect(store.conversation()?.pinnedNote).toBe('');
      expect(update).toHaveBeenCalledOnce();
      expect(await store.setPinnedNote('Explicit retry')).toBe(true);
      expect(update).toHaveBeenCalledTimes(2);
    }
  );

  it.each(['open', 'new', 'disconnect', 'destroy'] as const)(
    'settles an interrupted note creation immediately after %s without dispatching its note later',
    async (action) => {
      const created = deferred<Conversation>();
      vi.spyOn(api, 'createConversation').mockReturnValueOnce(created.promise);
      const saving = store.setPinnedNote('Original draft');
      if (action === 'open') await store.open('other');
      else if (action === 'new') store.startNew(true);
      else if (action === 'disconnect') store.connectionLost(new IpcError('unavailable', 'Offline'));
      else TestBed.resetTestingModule();
      expect(await saving).toBe(false);
      expect(store.savingNote()).toBe(false);
      const current = store.conversation();
      const failure = store.error();
      created.resolve(conversation('stale-created'));
      await Promise.resolve();
      expect(store.conversation()).toBe(current);
      expect(store.error()).toBe(failure);
      expect(api.patches).toEqual([]);
    }
  );

  it.each(['resolve', 'reject'] as const)(
    'does not let an invalidated note write %s replace another note or release its saving state',
    async (outcome) => {
      await store.open('old');
      const first = deferred<Conversation>();
      const second = deferred<Conversation>();
      const update = vi.spyOn(api, 'updateConversation').mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
      const old = store.setPinnedNote('Old note');
      await store.open('new');
      expect(await old).toBe(false);
      const fresh = store.setPinnedNote('New note');
      if (outcome === 'resolve') first.resolve(conversation('old', { pinnedNote: 'Stale note' }));
      else first.reject(new Error('Old failure'));
      await Promise.resolve();
      expect(store.savingNote()).toBe(true);
      expect(store.conversation()?.id).toBe('new');
      expect(store.conversation()?.pinnedNote).toBe('');
      expect(store.error()).toBeNull();
      second.resolve(conversation('new', { pinnedNote: 'New note' }));
      expect(await fresh).toBe(true);
      expect(store.savingNote()).toBe(false);
      expect(store.conversation()?.pinnedNote).toBe('New note');
      expect(update).toHaveBeenCalledTimes(2);
    }
  );

  it('blocks new writes to the same conversation after disconnect until the old native note write settles', async () => {
    await store.open('c1');
    const pending = deferred<Conversation>();
    const update = vi.spyOn(api, 'updateConversation').mockReturnValueOnce(pending.promise);
    const saving = store.setPinnedNote('Uncertain note');
    store.connectionLost(new IpcError('unavailable', 'Offline'));
    expect(await saving).toBe(false);
    expect(store.savingNote()).toBe(false);
    expect(await store.setPinnedNote('Must wait')).toBe(false);
    expect(update).toHaveBeenCalledOnce();
    pending.resolve(conversation('c1', { pinnedNote: 'Uncertain note' }));
    await Promise.resolve();
    expect(store.conversation()?.pinnedNote).toBe('');
    expect(await store.setPinnedNote('Latest explicit note')).toBe(true);
  });

  it('advances the editor session only for accepted new chats and successfully opened conversations', async () => {
    const initial = store.editorContext();
    store.startNew();
    expect(store.editorContext()).toBe(initial + 1);
    store.startNew(true);
    expect(store.editorContext()).toBe(initial + 2);
    const detail = await api.getConversation('saved');
    const opening = deferred<ConversationDetail>();
    const get = vi.spyOn(api, 'getConversation').mockReturnValueOnce(opening.promise);
    const pending = store.open('saved');
    expect(store.editorContext()).toBe(initial + 2);
    opening.resolve(detail);
    await pending;
    expect(store.editorContext()).toBe(initial + 3);
    get.mockRejectedValueOnce(new Error('Read failed'));
    await store.open('failed');
    expect(store.editorContext()).toBe(initial + 3);
    await store.rename('saved', 'New title');
    expect(store.editorContext()).toBe(initial + 3);
  });

  it('preserves the editor session when the first message or note creates its current conversation', async () => {
    const initial = store.editorContext();
    expect(await store.setPinnedNote('First note')).toBe(true);
    expect(store.editorContext()).toBe(initial);
    store.startNew();
    const draftSession = store.editorContext();
    await store.send('First message');
    expect(store.editorContext()).toBe(draftSession);
    store.startNew(); // Ignored while the reply is active.
    expect(store.editorContext()).toBe(draftSession);
    await store.open('ignored');
    expect(store.editorContext()).toBe(draftSession);
  });

  it('deletes the open conversation and starts a new chat', async () => {
    await store.open('c9');
    await store.remove('c9');
    expect(api.deleted).toEqual(['c9']);
    expect(store.conversation()).toBeNull();
    expect(store.entries()).toHaveLength(0);
  });

  it('keeps the active conversation on delete failure and permits retry', async () => {
    await store.open('c1');
    vi.spyOn(api, 'deleteConversation').mockRejectedValueOnce(new Error('Delete failed'));
    expect(await store.remove('c1')).toBe(false);
    expect(store.conversation()?.id).toBe('c1');
    expect(store.entries()).toHaveLength(2);
    expect(store.error()?.message).toBe('Delete failed');
    expect(store.deletingIds()).toEqual([]);
    expect(await store.remove('c1')).toBe('closed');
    expect(store.conversation()).toBeNull();
  });

  it('blocks duplicate deletes, metadata writes and sending in the deleting conversation', async () => {
    await store.open('c1');
    const response = deferred<void>();
    const remove = vi.spyOn(api, 'deleteConversation').mockReturnValueOnce(response.promise);
    const pending = store.remove('c1');
    expect(await store.remove('c1')).toBe(false);
    expect(await store.rename('c1', 'rename')).toBe(false);
    await store.send('message');
    expect(api.turns).toEqual([]);
    expect(remove).toHaveBeenCalledTimes(1);
    response.resolve();
    await pending;
    expect(store.deletingIds()).toEqual([]);
  });

  it('does not delete the active conversation while a turn is running', async () => {
    await store.send('message');
    expect(await store.remove('c1')).toBe(false);
    expect(api.deleted).toEqual([]);
    expect(store.busy()).toBe(true);
  });

  it('preserves a different pending open when deletion finishes', async () => {
    await store.open('old');
    const deleting = deferred<void>();
    vi.spyOn(api, 'deleteConversation').mockReturnValueOnce(deleting.promise);
    const pending = store.remove('old');
    const detail = await api.getConversation('new');
    const opening = deferred<ConversationDetail>();
    vi.spyOn(api, 'getConversation').mockReturnValueOnce(opening.promise);
    const open = store.open('new');
    deleting.resolve();
    expect(await pending).toBe('deleted');
    opening.resolve(detail);
    await open;
    expect(store.conversation()?.id).toBe('new');
  });

  it('advances the editor session when deletion empties the current chat before another open fails', async () => {
    await store.open('old');
    const initialEditor = store.editorContext();
    const deleting = deferred<void>();
    vi.spyOn(api, 'deleteConversation').mockReturnValueOnce(deleting.promise);
    const pending = store.remove('old');
    const opening = deferred<ConversationDetail>();
    vi.spyOn(api, 'getConversation').mockReturnValueOnce(opening.promise);
    const open = store.open('new');
    expect(store.editorContext()).toBe(initialEditor);
    deleting.resolve();
    expect(await pending).toBe('deleted');
    expect(store.conversation()).toBeNull();
    expect(store.entries()).toEqual([]);
    expect(store.editorContext()).toBe(initialEditor + 1);
    expect(store.loading()).toBe(true);
    opening.reject(new Error('New conversation cannot be opened'));
    await open;
    expect(store.loading()).toBe(false);
    expect(store.conversation()).toBeNull();
    expect(store.editorContext()).toBe(initialEditor + 1);
    expect(store.error()?.message).toBe('New conversation cannot be opened');
  });

  it('does not reopen a deleted conversation from an earlier read', async () => {
    const detail = await api.getConversation('deleted');
    const response = deferred<ConversationDetail>();
    vi.spyOn(api, 'getConversation').mockReturnValueOnce(response.promise);
    const pending = store.open('deleted');
    expect(await store.remove('deleted')).toBe('deleted');
    response.resolve(detail);
    await pending;
    expect(store.conversation()).toBeNull();
    expect(store.loading()).toBe(false);
  });

  it('ignores a delete failure after switching to another conversation', async () => {
    await store.open('old');
    const response = deferred<void>();
    vi.spyOn(api, 'deleteConversation').mockReturnValueOnce(response.promise);
    const pending = store.remove('old');
    await store.open('new');
    response.reject(new Error('Old delete failed'));
    expect(await pending).toBe(false);
    expect(store.conversation()?.id).toBe('new');
    expect(store.error()).toBeNull();
  });

  it('cancels the active request and marks the entry as stopped', async () => {
    await store.send('long task');
    api.emit(started());
    await store.cancel();
    expect(api.cancelled).toEqual(['req-1']);
    api.emit({ type: 'finished', finishReason: 'cancelled', usage: null, elapsedMs: 3 });
    expect(store.entries()[1].status).toBe('cancelled');
    expect(store.busy()).toBe(false);
  });

  it('records failures on the assistant entry', async () => {
    await store.send('hi');
    api.emit({
      type: 'failed',
      error: { code: 'timeout', message: 'AI request timed out' },
      elapsedMs: 5,
    });
    expect(store.entries()[1]).toMatchObject({ status: 'failed', error: 'AI request timed out' });

    api.sendError = new IpcError('limitExceeded', 'JARVIS is still answering', 'core');
    await store.send('again');
    expect(store.entries()[3]).toMatchObject({
      status: 'failed',
      error: 'JARVIS is still answering',
    });
    expect(store.busy()).toBe(false);
  });

  it('shares pending stop requests and keeps a failure visible until retry', async () => {
    await store.send('message');
    const response = deferred<boolean>();
    const cancel = vi.spyOn(api, 'cancelChat').mockReturnValueOnce(response.promise);
    const first = store.cancel();
    expect(store.cancel()).toBe(first);
    expect(cancel).toHaveBeenCalledTimes(1);
    expect(store.cancelling()).toBe(true);
    response.reject(new Error('Stop unavailable'));
    await first;
    await store.loadConversations();
    expect(store.cancelError()?.message).toBe('Stop unavailable');
    expect(store.busy()).toBe(true);
    expect(store.cancelling()).toBe(false);
    await store.cancel();
    expect(store.cancelError()).toBeNull();
    expect(store.busy()).toBe(true);
  });

  it('does not treat a refused stop as confirmed completion', async () => {
    await store.send('message');
    vi.spyOn(api, 'cancelChat').mockResolvedValueOnce(false);
    await store.cancel();
    expect(store.cancelError()?.message).toContain('no longer active');
    expect(store.entries()[1].status).toBe('streaming');
    api.emit({ type: 'finished', finishReason: 'stop', usage: null, elapsedMs: 1 });
    expect(store.cancelError()).toBeNull();
    expect(store.busy()).toBe(false);
  });

  it('ignores an old stop failure while a newer turn is being stopped', async () => {
    await store.send('first');
    const old = deferred<boolean>();
    const fresh = deferred<boolean>();
    vi.spyOn(api, 'cancelChat').mockReturnValueOnce(old.promise).mockReturnValueOnce(fresh.promise);
    const first = store.cancel();
    api.emit({ type: 'finished', finishReason: 'cancelled', usage: null, elapsedMs: 1 });
    await store.send('second');
    const second = store.cancel();
    old.reject(new Error('Old failure'));
    await first;
    expect(store.cancelling()).toBe(true);
    expect(store.cancelError()).toBeNull();
    fresh.resolve(true);
    await second;
    expect(store.cancelling()).toBe(false);
  });

  it('times out stopping and permits retry without applying late results', async () => {
    await store.send('message');
    const late = deferred<boolean>();
    vi.spyOn(api, 'cancelChat').mockReturnValueOnce(late.promise);
    vi.useFakeTimers();
    try {
      const pending = store.cancel();
      await vi.advanceTimersByTimeAsync(CHAT_CANCEL_TIMEOUT_MS);
      await pending;
      expect(store.cancelError()?.code).toBe('timeout');
      expect(store.cancelling()).toBe(false);
      await store.cancel();
      late.resolve(false);
      await Promise.resolve();
      expect(store.cancelError()).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it('sends an early stop once when started arrives before the send acknowledgement', async () => {
    const response = deferred<string>();
    let emit!: (event: ChatStreamEvent) => void;
    vi.spyOn(api, 'sendMessage').mockImplementationOnce((_request, onEvent) => {
      emit = onEvent;
      return response.promise;
    });
    const sending = store.send('message');
    await vi.waitFor(() => expect(emit).toBeDefined());
    await store.cancel();
    emit(started());
    response.resolve('req-1');
    await sending;
    expect(api.cancelled).toEqual(['req-1']);
  });

  it('settles pending cancellation on destruction without exposing errors', async () => {
    await store.send('message');
    vi.spyOn(api, 'cancelChat').mockReturnValueOnce(new Promise(() => undefined));
    const pending = store.cancel();
    TestBed.resetTestingModule();
    await pending;
    expect(store.cancelError()).toBeNull();
    expect(store.cancelling()).toBe(false);
  });

  it('ignores blank messages and messages sent while busy', async () => {
    await store.send('   ');
    expect(store.entries()).toHaveLength(0);
    await store.send('first');
    await store.send('second');
    expect(api.turns).toHaveLength(1);
  });

  it('preserves partial text on disconnect and ignores old stream events during a new turn', async () => {
    await store.send('first');
    api.emit(started());
    api.emit({ type: 'delta', text: 'partial answer' });
    const old = api.emit;
    store.connectionLost(new IpcError('unavailable', 'Offline'));
    expect(store.busy()).toBe(false);
    expect(store.entries()[1]).toMatchObject({ content: 'partial answer', status: 'failed' });
    expect(store.entries()[1].error).toContain('may still be running');
    expect(api.turns).toHaveLength(1);
    expect(api.cancelled).toEqual([]);
    await store.send('second');
    old(started());
    old({ type: 'delta', text: 'obsolete' });
    old({ type: 'finished', finishReason: 'stop', usage: null, elapsedMs: 1 });
    expect(store.phase()).toBe('connecting');
    expect(store.entries()[1].content).toBe('partial answer');
    expect(store.entries()[3].content).toBe('');
    api.emit(started());
    expect(store.phase()).toBe('streaming');
  });

  it.each(['resolve', 'reject'])('ignores a late send %s after its stream finished and another turn began', async (outcome) => {
    const response = deferred<string>();
    let old!: (event: ChatStreamEvent) => void;
    vi.spyOn(api, 'sendMessage').mockImplementationOnce((_request, onEvent) => {
      old = onEvent;
      return response.promise;
    });
    const onResult = vi.fn();
    const first = store.send('first', false, [], onResult);
    await vi.waitFor(() => expect(old).toBeDefined());
    old({ type: 'finished', finishReason: 'stop', usage: null, elapsedMs: 1 });
    expect(onResult).toHaveBeenCalledExactlyOnceWith('accepted');
    await store.send('second');
    if (outcome === 'resolve') response.resolve('obsolete-id');
    else response.reject(new Error('obsolete failure'));
    expect(await first).toBe('accepted');
    expect(onResult).toHaveBeenCalledOnce();
    expect(store.activeRequestId()).toBe('req-1');
    expect(store.phase()).toBe('connecting');
    expect(store.entries()[1].status).toBe('complete');
    expect(store.entries()[3].status).toBe('streaming');
  });

  it('does not send after a conversation creation completes following disconnect', async () => {
    const created = deferred<Conversation>();
    vi.spyOn(api, 'createConversation').mockReturnValueOnce(created.promise);
    const onResult = vi.fn();
    const pending = store.send('first', false, [], onResult);
    store.connectionLost(new IpcError('unavailable', 'Offline'));
    expect(onResult).toHaveBeenCalledExactlyOnceWith('notSent');
    expect(store.entries()[1].error).toBe('Connection lost: Offline The message was not sent.');
    store.startNew();
    created.resolve(conversation('late'));
    expect(await pending).toBe('notSent');
    expect(api.turns).toEqual([]);
    expect(store.conversation()).toBeNull();
    expect(store.entries()).toEqual([]);
    expect(onResult).toHaveBeenCalledOnce();
  });

  it('reports disconnect during admission as unknown before a late native refusal arrives', async () => {
    await store.open('c1');
    const response = deferred<string>();
    vi.spyOn(api, 'sendMessage').mockReturnValueOnce(response.promise);
    const onResult = vi.fn();
    const sending = store.send('Uncertain first turn', false, [], onResult);
    store.connectionLost(new IpcError('unavailable', 'Offline'));
    expect(onResult).toHaveBeenCalledExactlyOnceWith('unknown');
    await store.send('A new turn');
    const entries = store.entries();
    response.reject(new IpcError('permissionDenied', 'Old refusal', 'core'));
    expect(await sending).toBe('unknown');
    expect(store.entries()).toBe(entries);
    expect(store.phase()).toBe('connecting');
    expect(store.activeRequestId()).toBe('req-1');
    expect(onResult).toHaveBeenCalledOnce();
  });

  it.each(['creation', 'dispatch', 'streaming'] as const)(
    'settles an owned send once when destroyed during %s',
    async (stage) => {
      if (stage !== 'creation') await store.open('c1');
      const creation = deferred<Conversation>();
      const dispatch = deferred<string>();
      vi.spyOn(api, 'createConversation').mockReturnValueOnce(creation.promise);
      let emit!: (event: ChatStreamEvent) => void;
      const invoke = vi.spyOn(api, 'sendMessage').mockImplementationOnce((_request, onEvent) => {
        emit = onEvent;
        return dispatch.promise;
      });
      const onResult = vi.fn();
      const sending = store.send('Pending turn', false, [], onResult);
      if (stage === 'streaming') emit(started());
      TestBed.resetTestingModule();
      const expected = stage === 'creation' ? 'notSent' : stage === 'dispatch' ? 'unknown' : 'accepted';
      expect(onResult).toHaveBeenCalledExactlyOnceWith(expected);
      expect(await sending).toBe(expected);
      creation.resolve(conversation('late'));
      if (stage !== 'creation') dispatch.reject(new Error('Late native failure'));
      await Promise.resolve();
      expect(invoke).toHaveBeenCalledTimes(stage === 'creation' ? 0 : 1);
      expect(onResult).toHaveBeenCalledOnce();
    }
  );

  it('does not let a pending conversation read overwrite an explicitly started new chat', async () => {
    const detail = await api.getConversation('old');
    const response = deferred<ConversationDetail>();
    vi.spyOn(api, 'getConversation').mockReturnValueOnce(response.promise);
    const opening = store.open('old');
    store.startNew();
    await store.send('new turn');
    response.resolve(detail);
    await opening;
    expect(store.conversation()?.id).toBe('c1');
    expect(store.entries()[0].content).toBe('new turn');
    expect(store.loading()).toBe(false);
  });

  it('ignores stream callbacks and pending sends after destruction', async () => {
    await store.send('first');
    const before = store.entries();
    TestBed.resetTestingModule();
    api.emit(started());
    api.emit({ type: 'delta', text: 'late' });
    api.emit({ type: 'finished', finishReason: 'stop', usage: null, elapsedMs: 1 });
    await store.send('second');
    expect(store.entries()).toEqual(before);
    expect(api.turns).toHaveLength(1);
  });

  describe('saved messages checks', () => {
    async function uncertainTurn() {
      await store.send('Uncertain turn');
      api.emit(started());
      api.emit({ type: 'delta', text: 'Unsaved partial response' });
      store.connectionLost(new IpcError('unavailable', 'Connection closed'));
      expect(store.entries().at(-1)?.outcomeUnknown).toBe(true);
      expect(store.canCheckSaved()).toBe(true);
    }

    it('reads a separate snapshot without changing the transcript, draft context, note or captures', async () => {
      await uncertainTurn();
      const detail = await api.getConversation('c1');
      const get = vi.spyOn(api, 'getConversation').mockResolvedValueOnce(detail);
      const vision = TestBed.inject(VisionStore);
      vision.attachments.set([CAPTURE]);
      const capture = deferred<CaptureInfo>();
      api.captureScreen.mockReturnValueOnce(capture.promise);
      const capturing = vision.captureScreen(null);
      const entries = store.entries();
      const openConversation = store.conversation();
      const context = store.inputContext();
      const summary = store.summary();

      await store.checkSavedMessages();
      const nextCapture = { ...CAPTURE, id: 'captured-during-check' };
      capture.resolve(nextCapture);
      await capturing;

      expect(get).toHaveBeenCalledExactlyOnceWith('c1');
      expect(store.savedMessages()).toBe(detail);
      expect(store.entries()).toBe(entries);
      expect(store.conversation()).toBe(openConversation);
      expect(store.summary()).toBe(summary);
      expect(store.inputContext()).toBe(context);
      expect(vision.attachments()).toEqual([CAPTURE, nextCapture]);
      expect(api.discardCapture).not.toHaveBeenCalled();
      expect(store.entries().at(-1)).toMatchObject({ content: 'Unsaved partial response', outcomeUnknown: true });
      expect(api.turns).toHaveLength(1);
      expect(api.cancelled).toEqual([]);
      expect(store.canCheckSaved()).toBe(true);
    });

    it('coalesces repeated checks and keeps the action available while checking', async () => {
      await uncertainTurn();
      const detail = await api.getConversation('c1');
      const response = deferred<ConversationDetail>();
      const get = vi.spyOn(api, 'getConversation').mockReturnValueOnce(response.promise);
      const first = store.checkSavedMessages();
      const second = store.checkSavedMessages();
      expect(second).toBe(first);
      expect(get).toHaveBeenCalledOnce();
      expect(store.checkingSavedMessages()).toBe(true);
      expect(store.canCheckSaved()).toBe(true);
      response.resolve(detail);
      await first;
      expect(store.checkingSavedMessages()).toBe(false);
    });

    it('does not infer completion from missing saved messages or clear an uncertain response', async () => {
      await uncertainTurn();
      const entries = store.entries();
      const detail: ConversationDetail = { conversation: store.conversation()!, messages: [], summary: null };
      vi.spyOn(api, 'getConversation').mockResolvedValueOnce(detail);
      await store.checkSavedMessages();
      expect(store.savedMessages()?.messages).toEqual([]);
      expect(store.entries()).toBe(entries);
      expect(store.canCheckSaved()).toBe(true);
      expect(store.savedMessagesError()).toBeNull();
    });

    it('keeps the last successful snapshot if the next check fails and allows an explicit retry', async () => {
      await uncertainTurn();
      const detail = await api.getConversation('c1');
      const get = vi
        .spyOn(api, 'getConversation')
        .mockResolvedValueOnce(detail)
        .mockRejectedValueOnce(new Error('Read failed'))
        .mockResolvedValueOnce({ ...detail, messages: [] });
      await store.checkSavedMessages();
      await store.checkSavedMessages();
      expect(store.savedMessages()).toBe(detail);
      expect(store.savedMessagesError()?.message).toBe('Read failed');
      expect(store.error()).toBeNull();
      await store.checkSavedMessages();
      expect(get).toHaveBeenCalledTimes(3);
      expect(store.savedMessages()?.messages).toEqual([]);
      expect(store.savedMessagesError()).toBeNull();
    });

    it('refuses a snapshot for a different or temporary conversation', async () => {
      await uncertainTurn();
      const detail = await api.getConversation('c1');
      vi.spyOn(api, 'getConversation')
        .mockResolvedValueOnce({ ...detail, conversation: { ...detail.conversation, id: 'other' } })
        .mockResolvedValueOnce({ ...detail, conversation: { ...detail.conversation, temporary: true } });
      await store.checkSavedMessages();
      expect(store.savedMessages()).toBeNull();
      expect(store.savedMessagesError()?.message).toContain('do not belong');
      await store.checkSavedMessages();
      expect(store.savedMessages()).toBeNull();
      expect(store.savedMessagesError()?.message).toContain('do not belong');
    });

    it.each(['resolve', 'reject'] as const)('times out and ignores a late %s after a successful retry', async (outcome) => {
      await uncertainTurn();
      const detail = await api.getConversation('c1');
      const late = deferred<ConversationDetail>();
      const fresh = { ...detail, messages: [] };
      vi.spyOn(api, 'getConversation').mockReturnValueOnce(late.promise).mockResolvedValueOnce(fresh);
      vi.useFakeTimers();
      try {
        const first = store.checkSavedMessages();
        await vi.advanceTimersByTimeAsync(CONVERSATION_OPEN_TIMEOUT_MS);
        await first;
        expect(store.checkingSavedMessages()).toBe(false);
        expect(store.savedMessagesError()?.code).toBe('timeout');
        await store.checkSavedMessages();
        if (outcome === 'resolve') late.resolve(detail);
        else late.reject(new Error('Late failure'));
        await Promise.resolve();
        expect(store.savedMessages()).toBe(fresh);
        expect(store.savedMessagesError()).toBeNull();
      } finally {
        vi.useRealTimers();
      }
    });

    it.each(['send', 'open', 'new', 'delete', 'disconnect', 'destroy'] as const)(
      'invalidates a pending check when a subsequent %s action changes its context',
      async (action) => {
        await uncertainTurn();
        const detail = await api.getConversation('c1');
        const late = deferred<ConversationDetail>();
        vi.spyOn(api, 'getConversation').mockReturnValueOnce(late.promise);
        const pending = store.checkSavedMessages();
        if (action === 'send') await store.send('Next turn');
        else if (action === 'open') await store.open('other');
        else if (action === 'new') store.startNew();
        else if (action === 'delete') await store.remove('c1');
        else if (action === 'disconnect') store.connectionLost(new IpcError('unavailable', 'Offline again'));
        else TestBed.resetTestingModule();
        expect(store.savedMessages()).toBeNull();
        expect(store.checkingSavedMessages()).toBe(false);
        late.resolve(detail);
        await pending;
        expect(store.savedMessages()).toBeNull();
        expect(store.savedMessagesError()).toBeNull();
        expect(store.checkingSavedMessages()).toBe(false);
      }
    );

    it('does not let an invalidated check release a newer check pending after another connection loss', async () => {
      await uncertainTurn();
      const detail = await api.getConversation('c1');
      const first = deferred<ConversationDetail>();
      const second = deferred<ConversationDetail>();
      vi.spyOn(api, 'getConversation').mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
      const old = store.checkSavedMessages();
      store.connectionLost(new IpcError('unavailable', 'Offline again'));
      const fresh = store.checkSavedMessages();
      first.resolve(detail);
      await old;
      expect(store.checkingSavedMessages()).toBe(true);
      expect(store.savedMessages()).toBeNull();
      second.resolve(detail);
      await fresh;
      expect(store.checkingSavedMessages()).toBe(false);
      expect(store.savedMessages()).toBe(detail);
    });

    it('preserves a snapshot when a new send fails preflight without dispatching', async () => {
      await uncertainTurn();
      await store.checkSavedMessages();
      const saved = store.savedMessages();
      expect(await store.send(' ')).toBe('notSent');
      expect(store.savedMessages()).toBe(saved);
    });

    it.each(['temporary', 'busy', 'opening', 'savingNote', 'deleting', 'certain', 'noConversation', 'destroyed'] as const)(
      'does not read saved messages when the current conversation is %s',
      async (state) => {
        await uncertainTurn();
        const get = vi.spyOn(api, 'getConversation');
        if (state === 'temporary') store.conversation.update((item) => ({ ...item!, temporary: true }));
        else if (state === 'busy') store.phase.set('connecting');
        else if (state === 'opening') store.loading.set(true);
        else if (state === 'savingNote') store.savingNote.set(true);
        else if (state === 'deleting') store.deletingIds.set(['c1']);
        else if (state === 'certain')
          store.entries.update((entries) => entries.map((entry) => ({ ...entry, outcomeUnknown: false })));
        else if (state === 'noConversation') store.conversation.set(null);
        else TestBed.resetTestingModule();
        expect(store.canCheckSaved()).toBe(false);
        await store.checkSavedMessages();
        expect(get).not.toHaveBeenCalled();
      }
    );

    it('exposes the check for unknown dispatch failures but not confirmed refusals or creation failures', async () => {
      api.sendError = new IpcError('permissionDenied', 'Refused', 'core');
      await store.send('Known refusal');
      expect(store.canCheckSaved()).toBe(false);
      expect(store.entries().at(-1)?.outcomeUnknown).toBe(false);
      api.sendError = new IpcError('internal', 'Connection closed', 'transport');
      await store.send('Unknown dispatch');
      expect(store.canCheckSaved()).toBe(true);
      expect(store.entries().at(-1)?.outcomeUnknown).toBe(true);
      store.startNew();
      const create = deferred<Conversation>();
      vi.spyOn(api, 'createConversation').mockReturnValueOnce(create.promise);
      const sending = store.send('Not dispatched');
      store.connectionLost(new IpcError('unavailable', 'Offline'));
      create.resolve(conversation('late'));
      await sending;
      expect(store.canCheckSaved()).toBe(false);
      expect(store.entries().at(-1)?.outcomeUnknown).toBe(false);
    });
  });
});

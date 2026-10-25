import { TestBed } from '@angular/core/testing';
import type {
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
import { ChatStore, CHAT_CANCEL_TIMEOUT_MS, CONVERSATION_LIST_TIMEOUT_MS, CONVERSATION_OPEN_TIMEOUT_MS } from './chat.store';
import { SettingsStore } from './settings.store';

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
    expect(store.automaticRoute()?.model).toBe('echo');
    expect(store.diagnosticOnly()).toBe(true);
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

    api.sendError = new IpcError('limitExceeded', 'JARVIS is still answering');
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
    const first = store.send('first');
    await vi.waitFor(() => expect(old).toBeDefined());
    old({ type: 'finished', finishReason: 'stop', usage: null, elapsedMs: 1 });
    await store.send('second');
    if (outcome === 'resolve') response.resolve('obsolete-id');
    else response.reject(new Error('obsolete failure'));
    await first;
    expect(store.activeRequestId()).toBe('req-1');
    expect(store.phase()).toBe('connecting');
    expect(store.entries()[1].status).toBe('complete');
    expect(store.entries()[3].status).toBe('streaming');
  });

  it('does not send after a conversation creation completes following disconnect', async () => {
    const created = deferred<Conversation>();
    vi.spyOn(api, 'createConversation').mockReturnValueOnce(created.promise);
    const pending = store.send('first');
    store.connectionLost(new IpcError('unavailable', 'Offline'));
    store.startNew();
    created.resolve(conversation('late'));
    await pending;
    expect(api.turns).toEqual([]);
    expect(store.conversation()).toBeNull();
    expect(store.entries()).toEqual([]);
  });

  it('does not let a pending conversation read overwrite a new turn', async () => {
    const detail = await api.getConversation('old');
    const response = deferred<ConversationDetail>();
    vi.spyOn(api, 'getConversation').mockReturnValueOnce(response.promise);
    const opening = store.open('old');
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
});

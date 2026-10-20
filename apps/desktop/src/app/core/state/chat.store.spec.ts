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
import { ChatStore } from './chat.store';
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
    });
    expect(store.conversation()?.id).toBe('c1');
    expect(store.phase()).toBe('connecting');
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

  it('deletes the open conversation and starts a new chat', async () => {
    await store.open('c9');
    await store.remove('c9');
    expect(api.deleted).toEqual(['c9']);
    expect(store.conversation()).toBeNull();
    expect(store.entries()).toHaveLength(0);
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

  it('ignores blank messages and messages sent while busy', async () => {
    await store.send('   ');
    expect(store.entries()).toHaveLength(0);
    await store.send('first');
    await store.send('second');
    expect(api.turns).toHaveLength(1);
  });
});

import { TestBed } from '@angular/core/testing';
import type {
  ChatStartRequest,
  ChatStreamEvent,
  ProviderModels,
  RouteResolution,
} from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';
import { ChatStore } from './chat.store';

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

class FakeApi {
  connected = true;
  requests: ChatStartRequest[] = [];
  cancelled: string[] = [];
  emit: (event: ChatStreamEvent) => void = () => undefined;
  startError: IpcError | null = null;

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

  async startChat(
    request: ChatStartRequest,
    onEvent: (e: ChatStreamEvent) => void,
  ): Promise<string> {
    if (this.startError) {
      throw this.startError;
    }
    this.requests.push(request);
    this.emit = onEvent;
    return 'req-1';
  }

  async cancelChat(requestId: string): Promise<boolean> {
    this.cancelled.push(requestId);
    return true;
  }
}

describe('ChatStore', () => {
  let store: ChatStore;
  let api: FakeApi;

  beforeEach(async () => {
    api = new FakeApi();
    TestBed.configureTestingModule({ providers: [{ provide: JarvisApi, useValue: api }] });
    store = TestBed.inject(ChatStore);
    await store.loadModels();
  });

  it('defaults to automatic routing and flags diagnostic-only mode', () => {
    expect(store.selected()).toBeNull();
    expect(store.automaticRoute()?.model).toBe('echo');
    expect(store.diagnosticOnly()).toBe(true);
  });

  it('sends the explicitly selected model', async () => {
    store.select(store.modelOptions()[0]);
    expect(store.selected()?.model.id).toBe('echo');
    await store.send('hi');
    expect(api.requests[0]).toMatchObject({ providerId: 'diagnostic-echo', model: 'echo' });

    store.select(null);
    api.emit({ type: 'finished', finishReason: 'stop', usage: null, elapsedMs: 1 });
    await store.send('again');
    expect(api.requests[1]).toMatchObject({ providerId: null, model: null });
  });

  it('streams deltas into the assistant entry and returns to idle', async () => {
    await store.send('  hello  ');
    expect(api.requests[0]).toMatchObject({
      providerId: null,
      model: null,
      messages: [{ role: 'user', content: 'hello' }],
    });
    expect(store.phase()).toBe('connecting');

    api.emit({ type: 'started', requestId: 'req-1', providerId: 'diagnostic-echo', model: 'echo' });
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
    });
    expect(store.phase()).toBe('idle');
    expect(store.activeRequestId()).toBeNull();
  });

  it('sends previous turns as context', async () => {
    await store.send('first');
    api.emit({ type: 'delta', text: 'one' });
    api.emit({ type: 'finished', finishReason: 'stop', usage: null, elapsedMs: 1 });

    await store.send('second');
    expect(api.requests[1].messages).toEqual([
      { role: 'user', content: 'first' },
      { role: 'assistant', content: 'one' },
      { role: 'user', content: 'second' },
    ]);
  });

  it('cancels the active request and marks the entry as stopped', async () => {
    await store.send('long task');
    api.emit({ type: 'started', requestId: 'req-1', providerId: 'diagnostic-echo', model: 'echo' });
    await store.cancel();
    expect(api.cancelled).toEqual(['req-1']);

    api.emit({ type: 'finished', finishReason: 'cancelled', usage: null, elapsedMs: 3 });
    expect(store.entries()[1].status).toBe('cancelled');
    expect(store.busy()).toBe(false);
  });

  it('records stream failures on the assistant entry', async () => {
    await store.send('hi');
    api.emit({
      type: 'failed',
      error: { code: 'timeout', message: 'AI request timed out' },
      elapsedMs: 5,
    });
    expect(store.entries()[1]).toMatchObject({ status: 'failed', error: 'AI request timed out' });
    expect(store.phase()).toBe('idle');
  });

  it('records errors raised when starting a request', async () => {
    api.startError = new IpcError('limitExceeded', 'Too many requests');
    await store.send('hi');
    expect(store.entries()[1]).toMatchObject({ status: 'failed', error: 'Too many requests' });
    expect(store.busy()).toBe(false);
  });

  it('ignores blank messages and messages sent while busy', async () => {
    await store.send('   ');
    expect(store.entries()).toHaveLength(0);

    await store.send('first');
    await store.send('second');
    expect(api.requests).toHaveLength(1);
  });
});

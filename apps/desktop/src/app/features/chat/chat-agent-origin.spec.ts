import { TestBed } from '@angular/core/testing';
import { provideRouter, Router } from '@angular/router';
import type { AgentStartRequest, AgentTask, ConversationDetail, ProviderModels } from '@jarvis/shared-types';

import { JarvisApi } from '../../core/ipc/jarvis-api';
import { AgentsStore } from '../../core/state/agents.store';
import { ChatStore } from '../../core/state/chat.store';
import { MemoryStore } from '../../core/state/memory.store';
import { ModelsStore, optionKey } from '../../core/state/models.store';
import { agentTask } from '../../testing/agents.fixture';
import { Chat } from './chat';

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

function provider(id: string): ProviderModels {
  return {
    provider: { id, displayName: id, kind: 'localServer', locality: 'onDevice' },
    models: [
      {
        id: `${id}-model`,
        displayName: `${id} model`,
        providerId: id,
        contextWindow: 4096,
        capabilities: { chat: true, streaming: true, embeddings: false, toolCalling: false, vision: false },
      },
    ],
    error: null,
  };
}

function detail(id: string): ConversationDetail {
  return {
    conversation: {
      id,
      title: id,
      projectId: null,
      pinnedNote: '',
      temporary: false,
      messageCount: 0,
      createdAtMs: 1,
      updatedAtMs: 1,
    },
    messages: [],
    summary: null,
  };
}

describe('Chat agent launch origin', () => {
  async function render() {
    const api = {
      startAgentTask: vi
        .fn<(request: AgentStartRequest) => Promise<AgentTask>>()
        .mockResolvedValue(agentTask('created', 'pending')),
      listConversations: vi.fn(async () => []),
      getConversation: vi.fn(async (id: string) => detail(id)),
      discardCapture: vi.fn(async () => true),
    };
    TestBed.configureTestingModule({ providers: [provideRouter([]), { provide: JarvisApi, useValue: api }] });
    const store = TestBed.inject(ChatStore);
    const models = TestBed.inject(ModelsStore);
    const agents = TestBed.inject(AgentsStore);
    const start = vi.spyOn(agents, 'startTask');
    vi.spyOn(store, 'loadModels').mockResolvedValue();
    vi.spyOn(TestBed.inject(MemoryStore), 'loadProjects').mockResolvedValue();
    models.providers.set([provider('chosen'), provider('other')]);
    store.select(store.modelOptions()[0]);
    const router = TestBed.inject(Router);
    const navigate = vi.spyOn(router, 'navigate').mockResolvedValue(true);
    const fixture = TestBed.createComponent(Chat);
    await fixture.whenStable();
    fixture.detectChanges();
    const element = fixture.nativeElement as HTMLElement;
    const input = element.querySelector<HTMLTextAreaElement>('textarea[aria-label="Message"]')!;
    const type = (value: string, detect = true) => {
      input.value = value;
      input.dispatchEvent(new Event('input'));
      if (detect) fixture.detectChanges();
    };
    type('Inspect the workspace');
    const run = element.querySelector<HTMLButtonElement>('jv-chat-agent-action button')!;
    return { api, store, models, agents, start, router, navigate, fixture, element, input, type, run };
  }

  it('consumes only the confirmed submitted draft and opens the saved task', async () => {
    const { api, agents, fixture, input, run, navigate } = await render();
    run.click();
    await vi.waitFor(() => expect(navigate).toHaveBeenCalledExactlyOnceWith(['/agents'], { queryParams: { task: 'created' } }));
    fixture.detectChanges();
    expect(input.value).toBe('');
    expect(agents.selectedId()).toBe('created');
    expect(agents.tasks().map((task) => task.id)).toContain('created');
    expect(api.startAgentTask).toHaveBeenCalledExactlyOnceWith(
      expect.objectContaining({ goal: 'Inspect the workspace', providerId: 'chosen', model: 'chosen-model' })
    );
  });

  it.each(['new', 'open', 'pendingOpen'] as const)('does not leave a newer chat context after %s', async (action) => {
    const { api, store, agents, start, fixture, input, run, navigate } = await render();
    const pending = deferred<AgentTask>();
    api.startAgentTask.mockReturnValueOnce(pending.promise);
    run.click();
    const openingResult = deferred<ConversationDetail>();
    let opening: Promise<void> | undefined;
    if (action === 'new') store.startNew();
    else if (action === 'open') await store.open('another');
    else {
      api.getConversation.mockReturnValueOnce(openingResult.promise);
      opening = store.open('another');
    }
    pending.resolve(agentTask('old-context-task', 'pending'));
    await start.mock.results[0].value;
    fixture.detectChanges();
    expect(input.value).toBe('Inspect the workspace');
    expect(navigate).not.toHaveBeenCalled();
    expect(agents.selectedId()).toBeNull();
    expect(agents.tasks().map((task) => task.id)).toContain('old-context-task');
    expect(api.startAgentTask).toHaveBeenCalledOnce();
    if (opening) {
      openingResult.resolve(detail('another'));
      await opening;
    }
  });

  it('preserves a draft edited away and back before the old task is acknowledged', async () => {
    const { api, agents, start, fixture, input, run, navigate, type } = await render();
    const pending = deferred<AgentTask>();
    api.startAgentTask.mockReturnValueOnce(pending.promise);
    run.click();
    type('Newer draft', false);
    type('Inspect the workspace', false);
    pending.resolve(agentTask('old-revision-task', 'pending'));
    await start.mock.results[0].value;
    fixture.detectChanges();
    expect(input.value).toBe('Inspect the workspace');
    expect(navigate).not.toHaveBeenCalled();
    expect(agents.selectedId()).toBeNull();
    expect(agents.tasks().map((task) => task.id)).toContain('old-revision-task');
  });

  it.each(['note', 'fact'] as const)(
    'does not navigate away from a newer %s draft while task creation is pending',
    async (kind) => {
      const { api, store, agents, start, fixture, element, run, navigate } = await render();
      const pending = deferred<AgentTask>();
      api.startAgentTask.mockReturnValueOnce(pending.promise);
      run.click();
      let input: HTMLTextAreaElement;
      if (kind === 'note') {
        element.querySelector<HTMLButtonElement>('[aria-label="Pinned note"]')!.click();
        fixture.detectChanges();
        input = element.querySelector<HTMLTextAreaElement>('textarea[aria-label="Pinned note"]')!;
      } else {
        store.entries.set([{ id: 'remember-me', role: 'user', content: 'Fact from the conversation', status: 'complete' }]);
        fixture.detectChanges();
        element.querySelector<HTMLButtonElement>('button.remember')!.click();
        fixture.detectChanges();
        input = element.querySelector<HTMLTextAreaElement>('textarea[aria-label="Fact to remember"]')!;
      }
      input.value = 'New unsaved editor draft';
      input.dispatchEvent(new Event('input'));
      pending.resolve(agentTask('created-before-other-edit', 'pending'));
      await start.mock.results[0].value;
      fixture.detectChanges();
      expect(input.value).toBe('New unsaved editor draft');
      expect(navigate).not.toHaveBeenCalled();
      expect(agents.selectedId()).toBeNull();
      expect(agents.tasks().map((task) => task.id)).toContain('created-before-other-edit');
      expect(api.startAgentTask).toHaveBeenCalledOnce();
    }
  );

  it('refuses an old rendered model choice before dispatch and starts only after the updated choice renders', async () => {
    const { api, store, fixture, run, navigate } = await render();
    store.select(store.modelOptions()[1]);
    run.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    expect(api.startAgentTask).not.toHaveBeenCalled();
    fixture.detectChanges();
    run.click();
    await vi.waitFor(() => expect(navigate).toHaveBeenCalledOnce());
    expect(api.startAgentTask).toHaveBeenCalledExactlyOnceWith(
      expect.objectContaining({ providerId: 'other', model: 'other-model' })
    );
  });

  it('does not consume the draft after its model changes while the original task is starting', async () => {
    const { api, store, agents, start, fixture, input, run, navigate } = await render();
    const pending = deferred<AgentTask>();
    api.startAgentTask.mockReturnValueOnce(pending.promise);
    run.click();
    store.select(store.modelOptions()[1]);
    pending.resolve(agentTask('original-model-task', 'pending'));
    await start.mock.results[0].value;
    fixture.detectChanges();
    expect(input.value).toBe('Inspect the workspace');
    expect(navigate).not.toHaveBeenCalled();
    expect(agents.selectedId()).toBeNull();
    expect(api.startAgentTask).toHaveBeenCalledExactlyOnceWith(
      expect.objectContaining({ providerId: 'chosen', model: 'chosen-model' })
    );
  });

  it('preserves a new model-selection revision even when the user returns to the original model', async () => {
    const { api, agents, start, fixture, element, input, run, navigate } = await render();
    const pending = deferred<AgentTask>();
    api.startAgentTask.mockReturnValueOnce(pending.promise);
    run.click();
    const selector = element.querySelector<HTMLSelectElement>('.select.model select')!;
    for (const id of ['other', 'chosen']) {
      selector.value = optionKey(id, `${id}-model`);
      selector.dispatchEvent(new Event('change'));
    }
    pending.resolve(agentTask('before-model-edits', 'pending'));
    await start.mock.results[0].value;
    fixture.detectChanges();
    expect(input.value).toBe('Inspect the workspace');
    expect(navigate).not.toHaveBeenCalled();
    expect(agents.selectedId()).toBeNull();
    expect(api.startAgentTask).toHaveBeenCalledOnce();
  });

  it('does not return to a destroyed chat or modify the newly mounted composer', async () => {
    const { api, agents, start, fixture, run, navigate } = await render();
    const pending = deferred<AgentTask>();
    api.startAgentTask.mockReturnValueOnce(pending.promise);
    run.click();
    fixture.destroy();
    const next = TestBed.createComponent(Chat);
    await next.whenStable();
    next.detectChanges();
    const input = (next.nativeElement as HTMLElement).querySelector<HTMLTextAreaElement>('textarea[aria-label="Message"]')!;
    input.value = 'Draft in the new component';
    input.dispatchEvent(new Event('input'));
    pending.resolve(agentTask('created-after-close', 'pending'));
    await start.mock.results[0].value;
    next.detectChanges();
    expect(input.value).toBe('Draft in the new component');
    expect(navigate).not.toHaveBeenCalled();
    expect(agents.selectedId()).toBeNull();
    expect(agents.tasks().map((task) => task.id)).toContain('created-after-close');
  });

  it('keeps a created-task link if navigation fails after the confirmed draft is consumed', async () => {
    const { api, fixture, element, input, run, navigate } = await render();
    navigate.mockRejectedValueOnce(new Error('Cannot open task page'));
    run.click();
    await vi.waitFor(() => {
      fixture.detectChanges();
      expect(element.querySelector('jv-chat-agent-action [role="alert"]')?.textContent).toContain('Cannot open task page');
    });
    expect(input.value).toBe('');
    expect(element.querySelector('jv-chat-agent-action a')?.getAttribute('href')).toBe('/agents?task=created');
    expect(api.startAgentTask).toHaveBeenCalledOnce();
  });

  it('does not report an old navigation failure after an unrendered model change', async () => {
    const { fixture, element, run, navigate } = await render();
    let reject!: (error: unknown) => void;
    navigate.mockReturnValueOnce(
      new Promise((_resolve, fail) => {
        reject = fail;
      })
    );
    run.click();
    await vi.waitFor(() => expect(navigate).toHaveBeenCalledOnce());
    const selector = element.querySelector<HTMLSelectElement>('.select.model select')!;
    selector.value = optionKey('other', 'other-model');
    selector.dispatchEvent(new Event('change'));
    reject(new Error('Obsolete navigation failure'));
    await fixture.whenStable();
    fixture.detectChanges();
    expect(element.querySelector('jv-chat-agent-action [role="alert"]')).toBeNull();
    expect(element.textContent).not.toContain('Obsolete navigation failure');
  });

  it('does not supersede a pending user navigation with a late task acknowledgement', async () => {
    const { api, agents, start, router, navigate, fixture, input, run } = await render();
    const pending = deferred<AgentTask>();
    const guard = deferred<boolean>();
    let entered = false;
    api.startAgentTask.mockReturnValueOnce(pending.promise);
    router.resetConfig([
      {
        path: 'away',
        component: Chat,
        canActivate: [
          () => {
            entered = true;
            return guard.promise;
          },
        ],
      },
    ]);
    run.click();
    const leaving = router.navigateByUrl('/away');
    await vi.waitFor(() => expect(entered).toBe(true));
    pending.resolve(agentTask('created-while-leaving', 'pending'));
    await start.mock.results[0].value;
    fixture.detectChanges();
    expect(navigate).not.toHaveBeenCalled();
    expect(input.value).toBe('Inspect the workspace');
    expect(agents.selectedId()).toBeNull();
    expect(agents.tasks().map((task) => task.id)).toContain('created-while-leaving');
    guard.resolve(false);
    expect(await leaving).toBe(false);
    await fixture.whenStable();
  });
});

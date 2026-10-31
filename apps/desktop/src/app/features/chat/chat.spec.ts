import { TestBed } from '@angular/core/testing';
import { provideRouter, Router } from '@angular/router';
import type { ChatStreamEvent, ChatTurnRequest, Conversation, ConversationDetail, ProviderModels } from '@jarvis/shared-types';

import { JarvisApi } from '../../core/ipc/jarvis-api';
import { ChatStore } from '../../core/state/chat.store';
import { MemoryStore } from '../../core/state/memory.store';
import { ModelsStore, optionKey } from '../../core/state/models.store';
import { VisionStore } from '../../core/state/vision.store';
import { Chat } from './chat';

describe('Chat explicit model selection', () => {
  async function render() {
    const provider: ProviderModels = {
      provider: { id: 'chosen', displayName: 'Chosen server', kind: 'localServer', locality: 'onDevice' },
      models: [
        {
          id: 'model::v1',
          displayName: 'Chosen model',
          providerId: 'chosen',
          contextWindow: 4096,
          capabilities: { chat: true, streaming: true, vision: true, embeddings: false, toolCalling: false },
        },
      ],
      error: null,
    };
    const other: ProviderModels = {
      ...provider,
      provider: { id: 'other', displayName: 'Other server', kind: 'cloud', locality: 'thirdPartyCloud' },
      models: [{ ...provider.models[0], id: 'another', providerId: 'other' }],
    };
    const api = {
      createConversation: vi.fn(async (): Promise<Conversation> => ({
        id: 'created',
        title: 'New conversation',
        projectId: null,
        pinnedNote: '',
        temporary: false,
        messageCount: 0,
        createdAtMs: 1,
        updatedAtMs: 1,
      })),
      sendMessage: vi.fn<(request: ChatTurnRequest, onEvent: (event: ChatStreamEvent) => void) => Promise<string>>(
        async () => 'request'
      ),
      updateConversation: vi.fn(),
      getConversation: vi.fn(async (id: string): Promise<ConversationDetail> => ({
        conversation: {
          id,
          title: `Conversation ${id}`,
          projectId: null,
          pinnedNote: '',
          temporary: false,
          messageCount: 0,
          createdAtMs: 1,
          updatedAtMs: 1,
        },
        messages: [],
        summary: null,
      })),
      startAgentTask: vi.fn(),
      listConversations: vi.fn(async () => []),
      discardCapture: vi.fn(async () => true),
    };
    TestBed.configureTestingModule({ providers: [provideRouter([]), { provide: JarvisApi, useValue: api }] });
    const store = TestBed.inject(ChatStore);
    const models = TestBed.inject(ModelsStore);
    const vision = TestBed.inject(VisionStore);
    vi.spyOn(store, 'loadModels').mockResolvedValue();
    vi.spyOn(TestBed.inject(MemoryStore), 'loadProjects').mockResolvedValue();
    models.providers.set([provider, other]);
    store.select(store.modelOptions()[0]);
    const fixture = TestBed.createComponent(Chat);
    await fixture.whenStable();
    fixture.detectChanges();
    const element = fixture.nativeElement as HTMLElement;
    const input = element.querySelector<HTMLTextAreaElement>('textarea[aria-label="Message"]')!;
    input.value = 'Keep this draft';
    input.dispatchEvent(new Event('input'));
    vision.attachments.set([
      {
        id: 'capture',
        source: 'screen',
        label: 'Display 1',
        width: 100,
        height: 100,
        preview: 'data:image/jpeg;base64,AAAA',
        capturedAtMs: 1,
      },
    ]);
    fixture.detectChanges();
    const selector = element.querySelector<HTMLSelectElement>('.select.model select')!;
    const button = (label: string) =>
      [...element.querySelectorAll<HTMLButtonElement>('button')].find((item) => item.textContent?.trim() === label)!;
    return { provider, other, api, store, models, vision, fixture, element, input, selector, button };
  }

  it('navigates to a new conversation after its send acknowledgement in the original context', async () => {
    const { api, store, button } = await render();
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    button('Send').click();
    await vi.waitFor(() => expect(api.sendMessage).toHaveBeenCalledOnce());
    await vi.waitFor(() => expect(navigate).toHaveBeenCalledWith([], { queryParams: { c: 'created' }, replaceUrl: true }));
    expect(store.conversation()?.id).toBe('created');
    expect(navigate).toHaveBeenCalledOnce();
  });

  it.each(['open', 'new', 'destroy'] as const)(
    'ignores a late send acknowledgement after the completed stream and a subsequent %s action',
    async (action) => {
      const { api, store, fixture, button } = await render();
      const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
      const send = vi.spyOn(store, 'send');
      let acknowledge!: (id: string) => void;
      let emit!: (event: ChatStreamEvent) => void;
      api.sendMessage.mockImplementationOnce(
        (_request, onEvent) =>
          new Promise((resolve) => {
            emit = onEvent;
            acknowledge = resolve;
          })
      );
      button('Send').click();
      await vi.waitFor(() => expect(api.sendMessage).toHaveBeenCalledOnce());
      emit({ type: 'finished', finishReason: 'stop', usage: null, elapsedMs: 1 });
      expect(store.busy()).toBe(false);
      expect(store.conversation()?.id).toBe('created');

      let opening: Promise<void> | undefined;
      let finishOpen: ((detail: ConversationDetail) => void) | undefined;
      const next = await api.getConversation('next');
      if (action === 'open') {
        api.getConversation.mockReturnValueOnce(
          new Promise((resolve) => {
            finishOpen = resolve;
          })
        );
        opening = store.open('next');
        expect(store.loading()).toBe(true);
        expect(store.conversation()?.id).toBe('created');
      } else if (action === 'new') store.startNew();
      else fixture.destroy();

      acknowledge('old-request');
      await send.mock.results[0].value;
      expect(navigate).not.toHaveBeenCalled();
      if (action === 'open') {
        expect(store.loading()).toBe(true);
        finishOpen!(next);
        await opening;
        expect(store.conversation()?.id).toBe('next');
        expect(navigate).not.toHaveBeenCalled();
      } else if (action === 'new') expect(store.conversation()).toBeNull();
    }
  );

  it.each(['button', 'Enter'] as const)(
    'keeps the draft and captures when %s is used while another conversation opens',
    async (trigger) => {
      const { api, store, vision, fixture, input, button } = await render();
      await store.open('current');
      const next = await api.getConversation('next');
      let finish!: (detail: ConversationDetail) => void;
      api.getConversation.mockReturnValueOnce(
        new Promise((resolve) => {
          finish = resolve;
        })
      );
      const opening = store.open('next');
      // Opening clears the old captures; retain captures added for the next message.
      vision.attachments.set([
        {
          id: 'next-capture',
          source: 'screen',
          label: 'Display 1',
          width: 100,
          height: 100,
          preview: 'data:image/jpeg;base64,AAAA',
          capturedAtMs: 1,
        },
      ]);
      const pendingCaptures = vision.attachments();
      fixture.detectChanges();
      expect(button('Send').disabled).toBe(true);
      if (trigger === 'button') button('Send').dispatchEvent(new MouseEvent('click', { bubbles: true }));
      else input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
      expect(api.createConversation).not.toHaveBeenCalled();
      expect(api.sendMessage).not.toHaveBeenCalled();
      expect(store.conversation()?.id).toBe('current');
      expect(store.loading()).toBe(true);
      expect(input.value).toBe('Keep this draft');
      expect(vision.attachments()).toBe(pendingCaptures);

      finish(next);
      await opening;
      fixture.detectChanges();
      expect(button('Send').disabled).toBe(false);
      button('Send').click();
      await vi.waitFor(() => expect(api.sendMessage).toHaveBeenCalledOnce());
      fixture.detectChanges();
      expect(api.sendMessage).toHaveBeenCalledWith(
        expect.objectContaining({
          conversationId: 'next',
          content: 'Keep this draft',
          attachments: ['next-capture'],
        }),
        expect.any(Function)
      );
      expect(input.value).toBe('');
      expect(vision.attachments()).toEqual([]);
    }
  );

  it('keeps a pinned-note draft and blocks project changes until the requested conversation opens', async () => {
    const { api, store, fixture, element, button } = await render();
    await store.open('current');
    fixture.detectChanges();
    element.querySelector<HTMLButtonElement>('button[aria-label="Pinned note"]')!.click();
    fixture.detectChanges();
    const note = element.querySelector<HTMLTextAreaElement>('textarea[aria-label="Pinned note"]')!;
    note.value = 'Unsaved note for the current chat';
    note.dispatchEvent(new Event('input'));
    fixture.detectChanges();

    const next = await api.getConversation('next');
    let finish!: (detail: ConversationDetail) => void;
    api.getConversation.mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      })
    );
    const opening = store.open('next');
    fixture.detectChanges();
    const project = element.querySelector<HTMLSelectElement>('.controls .select:not(.model) select')!;
    expect(button('Save note').disabled).toBe(true);
    expect(project.disabled).toBe(true);
    button('Save note').dispatchEvent(new MouseEvent('click', { bubbles: true }));
    project.dispatchEvent(new Event('change'));
    await Promise.resolve();
    fixture.detectChanges();
    expect(api.updateConversation).not.toHaveBeenCalled();
    expect(api.createConversation).not.toHaveBeenCalled();
    expect(note.value).toBe('Unsaved note for the current chat');
    expect(element.querySelector('.note-actions .saved')).toBeNull();
    expect(store.conversation()?.id).toBe('current');
    expect(store.loading()).toBe(true);

    finish(next);
    await opening;
    fixture.detectChanges();
    expect(project.disabled).toBe(false);
    note.value = 'Note for the next chat';
    note.dispatchEvent(new Event('input'));
    fixture.detectChanges();
    api.updateConversation.mockResolvedValueOnce({ ...next.conversation, pinnedNote: 'Note for the next chat' });
    button('Save note').click();
    await vi.waitFor(() => expect(api.updateConversation).toHaveBeenCalledOnce());
    expect(api.updateConversation).toHaveBeenCalledWith(
      'next',
      expect.objectContaining({ pinnedNote: 'Note for the next chat' })
    );
  });

  it('retains an unavailable selection, draft and captures until the same model returns', async () => {
    const { provider, other, api, store, models, vision, fixture, element, input, selector, button } = await render();
    models.providers.set([other]);
    fixture.detectChanges();
    expect(selector.selectedOptions[0]?.textContent).toContain('Unavailable — model::v1');
    expect(selector.value).toBe(optionKey('chosen', 'model::v1'));
    expect(element.querySelector('[role="alert"]')?.textContent).toContain('selected model is unavailable');
    expect(button('Send').disabled).toBe(true);
    expect(button('Run as agent').disabled).toBe(true);
    button('Send').click();
    button('Run as agent').click();
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    expect(api.createConversation).not.toHaveBeenCalled();
    expect(api.sendMessage).not.toHaveBeenCalled();
    expect(api.startAgentTask).not.toHaveBeenCalled();
    expect(input.value).toBe('Keep this draft');
    expect(vision.attachments().map((capture) => capture.id)).toEqual(['capture']);

    // A missing/unknown option must not act like an explicit Automatic choice.
    selector.value = 'unknown';
    selector.dispatchEvent(new Event('change'));
    expect(store.selection()).toEqual({ providerId: 'chosen', model: 'model::v1' });
    models.providers.set([provider, other]);
    fixture.detectChanges();
    expect(store.selectionUnavailable()).toBe(false);
    expect(element.querySelector('[role="alert"]')).toBeNull();
    expect(button('Send').disabled).toBe(false);
    expect(button('Run as agent').disabled).toBe(false);
    expect(input.value).toBe('Keep this draft');
    expect(vision.attachments().map((capture) => capture.id)).toEqual(['capture']);
  });

  it('uses Automatic only after the user explicitly chooses it', async () => {
    const { other, api, store, models, vision, fixture, input, selector, button } = await render();
    models.providers.set([other]);
    fixture.detectChanges();
    selector.value = 'auto';
    selector.dispatchEvent(new Event('change'));
    fixture.detectChanges();
    expect(store.selection()).toBeNull();
    expect(store.selectionUnavailable()).toBe(false);
    expect(button('Send').disabled).toBe(false);
    button('Send').click();
    await vi.waitFor(() => expect(api.sendMessage).toHaveBeenCalledOnce());
    fixture.detectChanges();
    expect(api.sendMessage).toHaveBeenCalledWith(
      expect.objectContaining({
        content: 'Keep this draft',
        providerId: null,
        model: null,
        attachments: ['capture'],
      }),
      expect.any(Function)
    );
    expect(input.value).toBe('');
    expect(vision.attachments()).toEqual([]);
  });
});

describe('Chat pinned note', () => {
  it('disables Stop while pending and shows a retryable failure without ending the reply', async () => {
    let reject!: (reason: unknown) => void;
    const api = {
      sendMessage: vi.fn(async () => 'request'),
      cancelChat: vi.fn(
        () =>
          new Promise<boolean>((_resolve, fail) => {
            reject = fail;
          })
      ),
      listConversations: vi.fn(async () => []),
    };
    TestBed.configureTestingModule({ providers: [provideRouter([]), { provide: JarvisApi, useValue: api }] });
    const store = TestBed.inject(ChatStore);
    store.conversation.set({
      id: 'chat',
      title: 'Chat',
      projectId: null,
      pinnedNote: '',
      temporary: false,
      messageCount: 0,
      createdAtMs: 1,
      updatedAtMs: 1,
    });
    vi.spyOn(store, 'loadModels').mockResolvedValue();
    vi.spyOn(TestBed.inject(MemoryStore), 'loadProjects').mockResolvedValue();
    const fixture = TestBed.createComponent(Chat);
    await store.send('message');
    fixture.detectChanges();
    const element = fixture.nativeElement as HTMLElement;
    const stop = [...element.querySelectorAll('button')].find((button) => button.textContent?.trim() === 'Stop')!;
    stop.click();
    const pending = store.cancel();
    fixture.detectChanges();
    expect(stop.disabled).toBe(true);
    expect(stop.textContent).toContain('Stopping');
    reject(new Error('Stop transport failed'));
    await pending;
    fixture.detectChanges();
    expect(stop.disabled).toBe(false);
    expect(element.querySelector('[role="alert"]')?.textContent).toContain('Stop transport failed');
    expect(store.busy()).toBe(true);
    expect(api.cancelChat).toHaveBeenCalledTimes(1);
  });

  it('keeps edits made while a new conversation note is being saved', async () => {
    const created: Conversation = {
      id: 'created',
      title: 'New conversation',
      projectId: null,
      pinnedNote: '',
      temporary: false,
      messageCount: 0,
      createdAtMs: 1,
      updatedAtMs: 1,
    };
    let finish!: (value: Conversation) => void;
    const api = {
      createConversation: vi.fn(async () => created),
      updateConversation: vi.fn(
        () =>
          new Promise<Conversation>((resolve) => {
            finish = resolve;
          })
      ),
      listConversations: vi.fn(async () => []),
    };
    TestBed.configureTestingModule({ providers: [provideRouter([]), { provide: JarvisApi, useValue: api }] });
    const store = TestBed.inject(ChatStore);
    vi.spyOn(store, 'loadModels').mockResolvedValue();
    vi.spyOn(TestBed.inject(MemoryStore), 'loadProjects').mockResolvedValue();
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    const fixture = TestBed.createComponent(Chat);
    fixture.detectChanges();
    const element = fixture.nativeElement as HTMLElement;
    element.querySelector<HTMLButtonElement>('button[aria-label="Pinned note"]')!.click();
    fixture.detectChanges();
    const input = element.querySelector<HTMLTextAreaElement>('textarea[aria-label="Pinned note"]')!;
    input.value = 'Submitted note';
    input.dispatchEvent(new Event('input'));
    fixture.detectChanges();
    const save = [...element.querySelectorAll('button')].find((b) => b.textContent?.includes('Save note'))!;
    save.click();
    input.value = 'Newer draft';
    input.dispatchEvent(new Event('input'));
    await vi.waitFor(() => expect(api.updateConversation).toHaveBeenCalled());
    fixture.detectChanges();
    expect(save.disabled).toBe(true);
    expect(input.value).toBe('Newer draft');
    finish({ ...created, pinnedNote: 'Submitted note' });
    await vi.waitFor(() => expect(store.savingNote()).toBe(false));
    fixture.detectChanges();
    expect(input.value).toBe('Newer draft');
    expect(element.querySelector('.note-actions .saved')).toBeNull();
    expect(save.disabled).toBe(false);
    expect(navigate).not.toHaveBeenCalled();
  });
});

import { TestBed } from '@angular/core/testing';
import { provideRouter, Router } from '@angular/router';
import type { ChatStreamEvent, ChatTurnRequest, Conversation, ConversationDetail, ProviderModels } from '@jarvis/shared-types';

import { JarvisApi } from '../../core/ipc/jarvis-api';
import { IpcError } from '../../core/ipc/ipc-error';
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

  function savedConversation(content = 'The saved **answer**'): ConversationDetail {
    return {
      conversation: {
        id: 'created',
        title: 'Saved title',
        projectId: null,
        pinnedNote: 'Saved note',
        temporary: false,
        messageCount: 2,
        createdAtMs: 1,
        updatedAtMs: 2,
      },
      messages: [
        {
          id: 'saved-user',
          role: 'user',
          content: 'Keep this draft',
          status: 'complete',
          providerId: null,
          model: null,
          tokens: 4,
          createdAtMs: 1,
        },
        {
          id: 'saved-assistant',
          role: 'assistant',
          content,
          status: 'cancelled',
          providerId: 'chosen',
          model: 'model::v1',
          tokens: 4,
          createdAtMs: 2,
        },
      ],
      summary: null,
    };
  }

  async function uncertainChat() {
    const state = await render();
    state.api.sendMessage.mockRejectedValueOnce(new Error('Connection closed'));
    state.button('Send').click();
    await vi.waitFor(() => expect(state.store.canCheckSaved()).toBe(true));
    state.fixture.detectChanges();
    return state;
  }

  it('checks saved messages separately while preserving a partial reply, new draft, images and pinned-note edits', async () => {
    const { api, store, fixture, vision, element, input, button } = await render();
    let emit!: (event: ChatStreamEvent) => void;
    api.sendMessage.mockImplementationOnce(async (_request, onEvent) => {
      emit = onEvent;
      return 'request';
    });
    button('Send').click();
    await vi.waitFor(() => expect(api.sendMessage).toHaveBeenCalledOnce());
    emit({
      type: 'started',
      requestId: 'request',
      providerId: 'chosen',
      model: 'model::v1',
      conversationId: 'created',
      messageId: 'saved-assistant',
      context: null,
    });
    emit({ type: 'delta', text: 'An unfinished local reply' });
    store.connectionLost(new IpcError('unavailable', 'Offline'));
    const context = store.inputContext();
    const entries = store.entries();
    const conversation = store.conversation();
    const captures = [
      {
        id: 'new-image',
        source: 'screen' as const,
        label: 'New image',
        width: 100,
        height: 100,
        preview: 'data:image/jpeg;base64,AAAA',
        capturedAtMs: 2,
      },
    ];
    vision.attachments.set(captures);
    input.value = 'Next message';
    input.dispatchEvent(new Event('input'));
    element.querySelector<HTMLButtonElement>('[aria-label="Pinned note"]')!.click();
    fixture.detectChanges();
    const note = element.querySelector<HTMLTextAreaElement>('textarea[aria-label="Pinned note"]')!;
    note.value = 'Unsaved pinned-note edits';
    note.dispatchEvent(new Event('input'));
    api.getConversation.mockResolvedValueOnce(savedConversation());
    button('Check saved messages').click();
    await vi.waitFor(() => expect(store.savedMessages()).not.toBeNull());
    fixture.detectChanges();

    expect(element.querySelector('.saved-messages')?.textContent).toContain('The saved answer');
    expect(element.querySelector('.saved-messages strong')?.textContent).toBe('answer');
    expect(element.querySelector('.saved-messages')?.textContent).toContain('stopped');
    expect(element.querySelector('.messages')?.textContent).toContain('An unfinished local reply');
    expect(element.querySelector('.messages')?.textContent).toContain('may still be running');
    expect(store.entries()).toBe(entries);
    expect(store.conversation()).toBe(conversation);
    expect(store.inputContext()).toBe(context);
    expect(input.value).toBe('Next message');
    expect(note.value).toBe('Unsaved pinned-note edits');
    expect(vision.attachments()).toBe(captures);
    expect(api.getConversation).toHaveBeenCalledExactlyOnceWith('created');
    expect(api.sendMessage).toHaveBeenCalledOnce();
    expect(api.discardCapture).not.toHaveBeenCalled();
    expect(api.updateConversation).not.toHaveBeenCalled();
  });

  it('does not present an empty saved result as proof that the interrupted request ended', async () => {
    const { api, store, fixture, element, button } = await uncertainChat();
    api.getConversation.mockResolvedValueOnce({ ...savedConversation(), messages: [] });
    button('Check saved messages').click();
    await vi.waitFor(() => expect(store.savedMessages()).not.toBeNull());
    fixture.detectChanges();
    expect(element.querySelector('.saved-messages')?.textContent).toContain('No saved messages yet');
    expect(element.querySelector('.saved-messages')?.textContent).toContain('does not confirm whether the request ran');
    expect(button('Check again').disabled).toBe(false);
    expect(store.canCheckSaved()).toBe(true);
    expect(api.sendMessage).toHaveBeenCalledOnce();
  });

  it('keeps the previous saved preview on failure and retries only when Check again is pressed', async () => {
    const { api, store, fixture, element, button } = await uncertainChat();
    api.getConversation.mockResolvedValueOnce(savedConversation('First saved result'));
    button('Check saved messages').click();
    await vi.waitFor(() => expect(store.savedMessages()).not.toBeNull());
    fixture.detectChanges();
    api.getConversation.mockRejectedValueOnce({ code: 'io', message: 'Read failed' });
    button('Check again').click();
    await vi.waitFor(() => expect(store.savedMessagesError()?.message).toBe('Read failed'));
    fixture.detectChanges();
    expect(element.querySelector('.saved-check [role="alert"]')?.textContent).toContain('Read failed');
    expect(element.querySelector('.saved-messages')?.textContent).toContain('First saved result');
    expect(api.getConversation).toHaveBeenCalledTimes(2);
    api.getConversation.mockResolvedValueOnce(savedConversation('Later saved result'));
    button('Check again').click();
    await vi.waitFor(() => expect(store.savedMessages()?.messages[1].content).toBe('Later saved result'));
    fixture.detectChanges();
    expect(element.querySelector('.saved-check [role="alert"]')).toBeNull();
    expect(api.sendMessage).toHaveBeenCalledOnce();
  });

  it('shows pending checking state and does not reveal an old result after starting another conversation', async () => {
    const { api, store, fixture, element, button } = await uncertainChat();
    let complete!: (detail: ConversationDetail) => void;
    api.getConversation.mockReturnValueOnce(
      new Promise((resolve) => {
        complete = resolve;
      })
    );
    button('Check saved messages').click();
    fixture.detectChanges();
    expect(button('Checking…').disabled).toBe(true);
    expect(element.querySelector('.saved-check [role="status"]')?.textContent).toContain('Loading saved messages');
    button('Checking…').dispatchEvent(new MouseEvent('click', { bubbles: true }));
    expect(api.getConversation).toHaveBeenCalledOnce();
    store.startNew();
    complete(savedConversation('Old saved result'));
    await fixture.whenStable();
    fixture.detectChanges();
    expect(element.querySelector('.saved-check')).toBeNull();
    expect(element.textContent).not.toContain('Old saved result');
    expect(store.savedMessages()).toBeNull();
    expect(store.conversation()).toBeNull();
  });

  it('does not offer saved-result recovery for a confirmed refusal', async () => {
    const { api, store, fixture, element, button } = await render();
    api.sendMessage.mockRejectedValueOnce({ code: 'privacyBlocked', message: 'Cloud is disabled' });
    button('Send').click();
    await vi.waitFor(() => expect(store.entries().at(-1)?.status).toBe('failed'));
    fixture.detectChanges();
    expect(element.querySelector('.saved-check')).toBeNull();
    expect(api.getConversation).not.toHaveBeenCalled();
  });

  it('navigates to a new conversation after its send acknowledgement in the original context', async () => {
    const { api, store, button } = await render();
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    button('Send').click();
    await vi.waitFor(() => expect(api.sendMessage).toHaveBeenCalledOnce());
    await vi.waitFor(() => expect(navigate).toHaveBeenCalledWith([], { queryParams: { c: 'created' }, replaceUrl: true }));
    expect(store.conversation()?.id).toBe('created');
    expect(navigate).toHaveBeenCalledOnce();
  });

  it.each(['create', 'send'] as const)(
    'keeps the composer when the core rejects %s and sends again only on request',
    async (step) => {
      const { api, fixture, vision, input, button } = await render();
      const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
      const captures = vision.attachments();
      const failure = { code: 'permissionDenied', message: 'Sending is blocked' };
      if (step === 'create') api.createConversation.mockRejectedValueOnce(failure);
      else api.sendMessage.mockRejectedValueOnce(failure);

      button('Send').click();
      await vi.waitFor(() => {
        fixture.detectChanges();
        expect(button('Send').disabled).toBe(false);
        expect(fixture.nativeElement.textContent).toContain('Sending is blocked');
      });
      expect(input.value).toBe('Keep this draft');
      expect(vision.attachments()).toBe(captures);
      expect(navigate).not.toHaveBeenCalled();
      expect(api.sendMessage).toHaveBeenCalledTimes(step === 'create' ? 0 : 1);

      button('Send').click();
      await vi.waitFor(() => expect(api.sendMessage).toHaveBeenCalledTimes(step === 'create' ? 1 : 2));
      await vi.waitFor(() => {
        fixture.detectChanges();
        expect(input.value).toBe('');
      });
      expect(vision.attachments()).toEqual([]);
      expect(api.sendMessage).toHaveBeenLastCalledWith(
        expect.objectContaining({ content: 'Keep this draft', attachments: ['capture'] }),
        expect.any(Function)
      );
    }
  );

  it('keeps edits and new captures while admission of the previous draft is pending', async () => {
    const { api, fixture, vision, input, button } = await render();
    let accept!: (id: string) => void;
    api.sendMessage.mockReturnValueOnce(
      new Promise((resolve) => {
        accept = resolve;
      })
    );
    button('Send').click();
    await vi.waitFor(() => expect(api.sendMessage).toHaveBeenCalledOnce());
    fixture.detectChanges();
    expect(input.value).toBe('Keep this draft');
    expect(vision.attachments().map((capture) => capture.id)).toEqual(['capture']);

    // Even editing away and back is a new draft the acknowledgement must preserve.
    for (const value of ['Next draft', 'Keep this draft']) {
      input.value = value;
      input.dispatchEvent(new Event('input'));
    }
    const nextCapture = { ...vision.attachments()[0], id: 'next-capture' };
    vision.attachments.update((captures) => [...captures, nextCapture]);
    accept('request');
    await vi.waitFor(() => expect(vision.attachments()).toEqual([nextCapture]));
    fixture.detectChanges();
    expect(input.value).toBe('Keep this draft');
    expect(api.sendMessage).toHaveBeenCalledOnce();
  });

  it('consumes the admitted draft on an early event and preserves the next draft after late invoke failure', async () => {
    const { api, store, fixture, vision, input, button } = await render();
    let emit!: (event: ChatStreamEvent) => void;
    let reject!: (reason: unknown) => void;
    api.sendMessage.mockImplementationOnce(
      (_request, onEvent) =>
        new Promise((_resolve, fail) => {
          emit = onEvent;
          reject = fail;
        })
    );
    button('Send').click();
    await vi.waitFor(() => expect(api.sendMessage).toHaveBeenCalledOnce());
    emit({ type: 'failed', error: { code: 'privacyBlocked', message: 'Privacy was revoked' }, elapsedMs: 1 });
    fixture.detectChanges();
    expect(input.value).toBe('');
    expect(vision.attachments()).toEqual([]);
    input.value = 'My next message';
    input.dispatchEvent(new Event('input'));
    reject({ code: 'permissionDenied', message: 'Late response failure' });
    await vi.waitFor(() => expect(store.entries()[1].error).toBe('Privacy was revoked'));
    await fixture.whenStable();
    fixture.detectChanges();
    expect(input.value).toBe('My next message');
    expect(store.entries()[1].error).toBe('Privacy was revoked');
    expect(api.sendMessage).toHaveBeenCalledOnce();
  });

  it('does not leave an uncertain request ready for resending after a transport failure', async () => {
    const { api, fixture, vision, input, button } = await render();
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    api.sendMessage.mockRejectedValueOnce(new Error('Connection closed'));
    button('Send').click();
    await vi.waitFor(() => {
      fixture.detectChanges();
      expect(fixture.nativeElement.textContent).toContain('may still be running');
      expect(input.value).toBe('');
    });
    expect(vision.attachments()).toEqual([]);
    expect(api.sendMessage).toHaveBeenCalledOnce();
    expect(navigate).not.toHaveBeenCalled();
    expect(button('Send').disabled).toBe(true);
  });

  it('keeps text and images when the real desktop bridge is absent in an existing conversation', async () => {
    const { api, store, fixture, vision, input, button } = await render();
    await store.open('existing');
    const captures = [
      {
        id: 'remaining-capture',
        source: 'screen' as const,
        label: 'Display 1',
        width: 100,
        height: 100,
        preview: 'data:image/jpeg;base64,AAAA',
        capturedAtMs: 1,
      },
    ];
    vision.attachments.set(captures);
    const nativeApi = TestBed.runInInjectionContext(() => new JarvisApi());
    api.sendMessage.mockImplementationOnce((request, onEvent) => nativeApi.sendMessage(request, onEvent));
    fixture.detectChanges();
    button('Send').click();
    await vi.waitFor(() => {
      fixture.detectChanges();
      expect(fixture.nativeElement.textContent).toContain('core is not reachable');
    });
    expect(input.value).toBe('Keep this draft');
    expect(vision.attachments()).toBe(captures);
    expect(store.busy()).toBe(false);
    expect(button('Send').disabled).toBe(false);
    expect(api.sendMessage).toHaveBeenCalledOnce();
    expect(api.discardCapture).toHaveBeenCalledTimes(1); // Only opening discarded the old capture.
  });

  it('consumes submitted captures even if the chat component closes before admission', async () => {
    const { api, fixture, vision, button } = await render();
    let accept!: (id: string) => void;
    api.sendMessage.mockReturnValueOnce(
      new Promise((resolve) => {
        accept = resolve;
      })
    );
    button('Send').click();
    await vi.waitFor(() => expect(api.sendMessage).toHaveBeenCalledOnce());
    fixture.destroy();
    accept('request');
    await vi.waitFor(() => expect(vision.attachments()).toEqual([]));
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
      await fixture.whenStable();
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

  it('links a reply blocked by privacy settings to them, and only that one', async () => {
    const { api, fixture, element, input, button } = await render();
    const link = () => element.querySelector<HTMLAnchorElement>('.error-text a');
    api.sendMessage.mockImplementationOnce(async (_request, onEvent) => {
      onEvent({
        type: 'failed',
        error: { code: 'privacyBlocked', message: 'Sending data to the cloud provider "Claude" is disabled in privacy settings' },
        elapsedMs: 1,
      });
      return 'request';
    });
    button('Send').click();
    await vi.waitFor(() => {
      fixture.detectChanges();
      expect(link()?.getAttribute('href')).toBe('/settings');
    });
    expect(link()?.textContent).toContain('Privacy');

    api.sendMessage.mockRejectedValueOnce({ code: 'permissionDenied', message: 'Blocked by the security policy' });
    input.value = 'Again';
    input.dispatchEvent(new Event('input'));
    fixture.detectChanges();
    button('Send').click();
    await vi.waitFor(() => {
      fixture.detectChanges();
      expect(element.textContent).toContain('Blocked by the security policy');
    });
    const errors = [...element.querySelectorAll('.error-text')];
    expect(errors.at(-1)?.querySelector('a')).toBeNull();
  });
});

describe('Chat pinned note', () => {
  async function editor() {
    const created: Conversation = {
      id: 'note-created',
      title: 'New conversation',
      projectId: null,
      pinnedNote: '',
      temporary: false,
      messageCount: 0,
      createdAtMs: 1,
      updatedAtMs: 1,
    };
    const api = {
      createConversation: vi.fn(async () => created),
      updateConversation: vi.fn(async (_id: string, patch: { pinnedNote: string | null }) => ({
        ...created,
        pinnedNote: patch.pinnedNote ?? '',
      })),
      getConversation: vi.fn(async (id: string): Promise<ConversationDetail> => ({
        conversation: { ...created, id, pinnedNote: `Saved note for ${id}` },
        messages: [],
        summary: null,
      })),
      sendMessage: vi.fn(async () => 'request'),
      listConversations: vi.fn(async () => []),
      discardCapture: vi.fn(async () => true),
      deleteConversation: vi.fn(async () => undefined),
    };
    TestBed.configureTestingModule({ providers: [provideRouter([]), { provide: JarvisApi, useValue: api }] });
    const store = TestBed.inject(ChatStore);
    vi.spyOn(store, 'loadModels').mockResolvedValue();
    vi.spyOn(TestBed.inject(MemoryStore), 'loadProjects').mockResolvedValue();
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    const fixture = TestBed.createComponent(Chat);
    await fixture.whenStable();
    fixture.detectChanges();
    const element = fixture.nativeElement as HTMLElement;
    element.querySelector<HTMLButtonElement>('[aria-label="Pinned note"]')!.click();
    fixture.detectChanges();
    const note = element.querySelector<HTMLTextAreaElement>('textarea[aria-label="Pinned note"]')!;
    const typeNote = (value: string) => {
      note.value = value;
      note.dispatchEvent(new Event('input'));
      fixture.detectChanges();
    };
    const save = [...element.querySelectorAll<HTMLButtonElement>('button')].find(
      (button) => button.textContent?.trim() === 'Save note'
    )!;
    return { api, store, fixture, element, note, typeNote, save, created, navigate };
  }

  it('keeps an unsaved pinned note when the first message creates the conversation', async () => {
    const { api, store, fixture, element, note, typeNote } = await editor();
    typeNote('Unsaved working note');
    const input = element.querySelector<HTMLTextAreaElement>('textarea[aria-label="Message"]')!;
    input.value = 'First message';
    input.dispatchEvent(new Event('input'));
    fixture.detectChanges();
    [...element.querySelectorAll<HTMLButtonElement>('button')].find((button) => button.textContent?.trim() === 'Send')!.click();
    await vi.waitFor(() => expect(api.sendMessage).toHaveBeenCalledOnce());
    await fixture.whenStable();
    fixture.detectChanges();
    expect(store.conversation()?.id).toBe('note-created');
    expect(note.value).toBe('Unsaved working note');
    expect(store.conversation()?.pinnedNote).toBe('');
    expect(api.updateConversation).not.toHaveBeenCalled();
  });

  it.each([false, true])('starts a fresh pinned-note editor for a new unsaved chat (temporary: %s)', async (temporary) => {
    const { store, fixture, note, typeNote } = await editor();
    typeNote('Note from the previous unsaved chat');
    expect(store.conversation()).toBeNull();
    store.startNew(temporary);
    fixture.detectChanges();
    expect(store.conversation()).toBeNull();
    expect(note.value).toBe('');
  });

  it('keeps the note during a failed open and loads another note only after a successful open', async () => {
    const { api, store, fixture, note, typeNote } = await editor();
    typeNote('Unsent note for this new chat');
    let reject!: (reason: unknown) => void;
    api.getConversation.mockReturnValueOnce(
      new Promise((_resolve, fail) => {
        reject = fail;
      })
    );
    const opening = store.open('unavailable');
    fixture.detectChanges();
    expect(note.value).toBe('Unsent note for this new chat');
    reject(new Error('Read failed'));
    await opening;
    fixture.detectChanges();
    expect(note.value).toBe('Unsent note for this new chat');
    await store.open('other');
    fixture.detectChanges();
    expect(note.value).toBe('Saved note for other');
  });

  it('does not report a note as saved or navigate after an edit away and back during the save', async () => {
    const { api, store, fixture, element, note, typeNote, save, created, navigate } = await editor();
    let complete!: (value: Conversation) => void;
    api.updateConversation.mockReturnValueOnce(
      new Promise((resolve) => {
        complete = resolve;
      })
    );
    typeNote('Submitted note');
    save.click();
    await vi.waitFor(() => expect(api.updateConversation).toHaveBeenCalledOnce());
    typeNote('Newer edits');
    typeNote('Submitted note');
    complete({ ...created, pinnedNote: 'Submitted note' });
    await vi.waitFor(() => expect(store.savingNote()).toBe(false));
    await fixture.whenStable();
    fixture.detectChanges();
    expect(note.value).toBe('Submitted note');
    expect(element.querySelector('.note-actions .saved')).toBeNull();
    expect(navigate).not.toHaveBeenCalled();
  });

  it('does not navigate back when a note save succeeds after the chat component closes', async () => {
    const { api, store, fixture, typeNote, save, created, navigate } = await editor();
    let complete!: (value: Conversation) => void;
    api.updateConversation.mockReturnValueOnce(
      new Promise((resolve) => {
        complete = resolve;
      })
    );
    typeNote('Submitted note');
    save.click();
    await vi.waitFor(() => expect(api.updateConversation).toHaveBeenCalledOnce());
    fixture.destroy();
    complete({ ...created, pinnedNote: 'Submitted note' });
    await vi.waitFor(() => expect(store.savingNote()).toBe(false));
    expect(store.conversation()?.pinnedNote).toBe('Submitted note');
    expect(navigate).not.toHaveBeenCalled();
  });

  it('confirms and links a successfully saved note in the original editor', async () => {
    const { api, store, fixture, element, note, typeNote, save, navigate } = await editor();
    typeNote('Saved working note');
    save.click();
    await vi.waitFor(() => expect(store.savingNote()).toBe(false));
    await fixture.whenStable();
    fixture.detectChanges();
    expect(note.value).toBe('Saved working note');
    expect(element.querySelector('.note-actions .saved')?.textContent).toContain('Saved');
    expect(api.updateConversation).toHaveBeenCalledOnce();
    expect(navigate).toHaveBeenCalledExactlyOnceWith([], { queryParams: { c: 'note-created' } });
  });

  it('ignores a stale note-save action after a new editor opens before rendering', async () => {
    const { api, store, typeNote, save } = await editor();
    typeNote('Note from the old editor');
    store.startNew(false, 'new-project');
    save.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    await Promise.resolve();
    expect(api.createConversation).not.toHaveBeenCalled();
    expect(api.updateConversation).not.toHaveBeenCalled();
    expect(store.conversation()).toBeNull();
    expect(store.projectId()).toBe('new-project');
  });

  it('blocks project changes while a note-created conversation is awaiting confirmation', async () => {
    const { api, store, fixture, element, typeNote, save, created } = await editor();
    let complete!: (value: Conversation) => void;
    api.createConversation.mockReturnValueOnce(
      new Promise((resolve) => {
        complete = resolve;
      })
    );
    typeNote('Working note');
    save.click();
    fixture.detectChanges();
    const project = element.querySelector<HTMLSelectElement>('.controls .select:not(.model) select')!;
    expect(project.disabled).toBe(true);
    project.dispatchEvent(new Event('change', { bubbles: true }));
    await store.setProject('must-not-change');
    expect(store.projectId()).toBeNull();
    complete(created);
    await vi.waitFor(() => expect(store.savingNote()).toBe(false));
    fixture.detectChanges();
    expect(project.disabled).toBe(false);
    expect(api.updateConversation).toHaveBeenCalledOnce();
  });

  it('clears the deleted conversation note even when a pending replacement conversation fails to open', async () => {
    const { api, store, fixture, note, typeNote } = await editor();
    await store.open('old');
    fixture.detectChanges();
    typeNote('Unsent note from the deleted conversation');
    let deleted!: () => void;
    let rejectOpen!: (reason: unknown) => void;
    api.deleteConversation.mockReturnValueOnce(
      new Promise((resolve) => {
        deleted = () => resolve(undefined);
      })
    );
    api.getConversation.mockReturnValueOnce(
      new Promise((_resolve, reject) => {
        rejectOpen = reject;
      })
    );
    const removing = store.remove('old');
    const opening = store.open('next');
    deleted();
    await removing;
    fixture.detectChanges();
    expect(store.conversation()).toBeNull();
    expect(note.value).toBe('');
    rejectOpen(new Error('Next conversation unavailable'));
    await opening;
    fixture.detectChanges();
    expect(note.value).toBe('');
    expect(store.conversation()).toBeNull();
    expect(api.createConversation).not.toHaveBeenCalled();
    expect(api.updateConversation).not.toHaveBeenCalled();
  });

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

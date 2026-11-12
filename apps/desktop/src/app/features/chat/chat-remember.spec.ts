import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import type { Conversation, ConversationDetail, Fact } from '@jarvis/shared-types';

import { JarvisApi } from '../../core/ipc/jarvis-api';
import { ChatStore, type ChatEntry } from '../../core/state/chat.store';
import { MemoryStore } from '../../core/state/memory.store';
import { Chat } from './chat';

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((complete) => {
    resolve = complete;
  });
  return { promise, resolve };
}

function conversation(id: string, temporary = false): Conversation {
  return {
    id,
    title: id,
    projectId: `project-${id}`,
    pinnedNote: '',
    temporary,
    messageCount: 2,
    createdAtMs: 1,
    updatedAtMs: 1,
  };
}

function savedFact(): Fact {
  return {
    id: 'saved-fact',
    content: 'First fact',
    projectId: 'project-first',
    pinned: false,
    source: 'conversation',
    sourceConversationId: 'first',
    createdAtMs: 1,
    updatedAtMs: 1,
  };
}

describe('Chat Remember editor', () => {
  async function render(temporary = false) {
    const api = {
      getConversation: vi.fn(async (id: string): Promise<ConversationDetail> => ({
        conversation: conversation(id),
        messages: [
          {
            id: `${id}-entry`,
            role: 'user',
            content: `Message in ${id}`,
            status: 'complete',
            providerId: null,
            model: null,
            tokens: 4,
            createdAtMs: 1,
          },
        ],
        summary: null,
      })),
      discardCapture: vi.fn(async () => true),
    };
    TestBed.configureTestingModule({ providers: [provideRouter([]), { provide: JarvisApi, useValue: api }] });
    const store = TestBed.inject(ChatStore);
    const memory = TestBed.inject(MemoryStore);
    vi.spyOn(store, 'loadModels').mockResolvedValue();
    vi.spyOn(store, 'loadConversations').mockResolvedValue();
    vi.spyOn(memory, 'loadProjects').mockResolvedValue();
    store.conversation.set(conversation('first', temporary));
    const entries: readonly ChatEntry[] = [
      { id: 'first-entry', role: 'user', content: 'First fact', status: 'complete' },
      { id: 'second-entry', role: 'assistant', content: 'Second fact', status: 'complete' },
    ];
    store.entries.set(entries);
    const save = vi.spyOn(memory, 'saveFact');
    const fixture = TestBed.createComponent(Chat);
    await fixture.whenStable();
    fixture.detectChanges();
    const element = fixture.nativeElement as HTMLElement;
    const rows = () => [...element.querySelectorAll<HTMLElement>('.messages .message')];
    const button = (label: string) =>
      [...element.querySelectorAll<HTMLButtonElement>('.remember-editor button')].find(
        (item) => item.textContent?.trim() === label
      )!;
    const editor = () => element.querySelector<HTMLTextAreaElement>('textarea[aria-label="Fact to remember"]');
    const open = (index: number) => {
      rows()[index].querySelector<HTMLButtonElement>('button.remember')!.click();
      fixture.detectChanges();
    };
    const type = (content: string) => {
      editor()!.value = content;
      editor()!.dispatchEvent(new Event('input'));
      fixture.detectChanges();
    };
    return { api, store, memory, fixture, element, rows, save, button, editor, open, type };
  }

  it.each([false, true])(
    'saves the selected fact with its project and appropriate conversation source (temporary: %s)',
    async (temporary) => {
      const { fixture, save, rows, open, button, editor } = await render(temporary);
      save.mockResolvedValueOnce(savedFact());
      open(0);
      button('Save fact').click();
      await fixture.whenStable();
      fixture.detectChanges();
      expect(save).toHaveBeenCalledExactlyOnceWith({
        id: null,
        content: 'First fact',
        projectId: 'project-first',
        pinned: false,
        sourceConversationId: temporary ? null : 'first',
      });
      expect(editor()).toBeNull();
      expect(rows()[0].querySelector('.saved')?.textContent).toContain('Remembered');
    }
  );

  it('preserves a different message editor when an earlier fact finishes saving', async () => {
    const { fixture, save, rows, open, type, button, editor } = await render();
    const pending = deferred<Fact | null>();
    save.mockReturnValueOnce(pending.promise);
    open(0);
    button('Save fact').click();
    open(1);
    type('New draft for the second message');
    expect(button('Save fact').disabled).toBe(true);
    pending.resolve(savedFact());
    await fixture.whenStable();
    fixture.detectChanges();
    expect(editor()?.value).toBe('New draft for the second message');
    expect(rows()[1].querySelector('.remember-editor')).not.toBeNull();
    expect(button('Save fact').disabled).toBe(false);
    expect(save).toHaveBeenCalledOnce();
  });

  it('keeps a newer text revision even when edited back to the submitted text', async () => {
    const { fixture, save, open, type, button, editor, rows } = await render();
    const pending = deferred<Fact | null>();
    save.mockReturnValueOnce(pending.promise);
    open(0);
    button('Save fact').click();
    type('Different text');
    type('First fact');
    pending.resolve(savedFact());
    await fixture.whenStable();
    fixture.detectChanges();
    expect(editor()?.value).toBe('First fact');
    expect(rows()[0].querySelector('.saved')).toBeNull();
    expect(save).toHaveBeenCalledOnce();
  });

  it('does not close a cancelled and reopened editor after its earlier save completes', async () => {
    const { fixture, save, open, type, button, editor } = await render();
    const pending = deferred<Fact | null>();
    save.mockReturnValueOnce(pending.promise);
    open(0);
    button('Save fact').click();
    button('Cancel').click();
    fixture.detectChanges();
    open(0);
    type('A reopened draft');
    pending.resolve(savedFact());
    await fixture.whenStable();
    fixture.detectChanges();
    expect(editor()?.value).toBe('A reopened draft');
    expect(button('Save fact').disabled).toBe(false);
    expect(save).toHaveBeenCalledOnce();
  });

  it('refuses a stale Save fact click after an explicit new conversation starts', async () => {
    const { store, fixture, save, open, type, button, editor } = await render();
    open(0);
    type('Belongs to the old conversation');
    const oldSave = button('Save fact');
    store.startNew(false, 'another-project');
    // Invoke the rendered listener before Angular has replaced the old editor.
    oldSave.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    fixture.detectChanges();
    expect(save).not.toHaveBeenCalled();
    expect(editor()).toBeNull();
    expect(store.projectId()).toBe('another-project');
  });

  it('preserves an editor in another conversation when an old fact save finishes', async () => {
    const { store, fixture, save, open, type, button, editor } = await render();
    const pending = deferred<Fact | null>();
    save.mockReturnValueOnce(pending.promise);
    open(0);
    button('Save fact').click();
    await store.open('next');
    fixture.detectChanges();
    open(0);
    type('Fact for the next conversation');
    pending.resolve(savedFact());
    await fixture.whenStable();
    fixture.detectChanges();
    expect(editor()?.value).toBe('Fact for the next conversation');
    expect(save).toHaveBeenCalledExactlyOnceWith(
      expect.objectContaining({
        content: 'First fact',
        sourceConversationId: 'first',
        projectId: 'project-first',
      })
    );
  });

  it('does not let a disposed chat editor interfere with a new chat component', async () => {
    const { fixture, save, open, button } = await render();
    const pending = deferred<Fact | null>();
    save.mockReturnValueOnce(pending.promise);
    open(0);
    button('Save fact').click();
    fixture.destroy();
    const nextFixture = TestBed.createComponent(Chat);
    await nextFixture.whenStable();
    nextFixture.detectChanges();
    const nextElement = nextFixture.nativeElement as HTMLElement;
    nextElement.querySelector<HTMLButtonElement>('.message button.remember')!.click();
    nextFixture.detectChanges();
    const nextEditor = nextElement.querySelector<HTMLTextAreaElement>('textarea[aria-label="Fact to remember"]')!;
    nextEditor.value = 'Draft in the new component';
    nextEditor.dispatchEvent(new Event('input'));
    pending.resolve(savedFact());
    await nextFixture.whenStable();
    nextFixture.detectChanges();
    expect(nextEditor.value).toBe('Draft in the new component');
    expect(nextElement.querySelector('.remember-editor')).not.toBeNull();
    expect(nextElement.querySelector('.message .saved')).toBeNull();
    expect(save).toHaveBeenCalledOnce();
  });

  it('coalesces a forced duplicate click while a fact save is pending', async () => {
    const { fixture, save, open, button } = await render();
    const pending = deferred<Fact | null>();
    save.mockReturnValueOnce(pending.promise);
    open(0);
    const saveButton = button('Save fact');
    saveButton.click();
    fixture.detectChanges();
    expect(saveButton.disabled).toBe(true);
    saveButton.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    expect(save).toHaveBeenCalledOnce();
    pending.resolve(savedFact());
    await fixture.whenStable();
  });
});

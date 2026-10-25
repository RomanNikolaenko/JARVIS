import { TestBed } from '@angular/core/testing';
import { provideRouter, Router } from '@angular/router';
import type { Conversation } from '@jarvis/shared-types';

import { JarvisApi } from '../../../core/ipc/jarvis-api';
import { ChatStore } from '../../../core/state/chat.store';
import { ConversationList } from './conversation-list';

describe('ConversationList', () => {
  function setup() {
    const api = {
      listConversations: vi.fn(async (): Promise<Conversation[]> => []),
      deleteConversation: vi.fn(async () => undefined),
    };
    TestBed.configureTestingModule({
      imports: [ConversationList],
      providers: [provideRouter([]), { provide: JarvisApi, useValue: api }],
    });
    return { api, store: TestBed.inject(ChatStore), fixture: TestBed.createComponent(ConversationList) };
  }

  it('shows loading and search failure, then retries without claiming an empty result', async () => {
    const { api, store, fixture } = setup();
    let reject!: (reason: unknown) => void;
    api.listConversations.mockReturnValueOnce(
      new Promise((_resolve, fail) => {
        reject = fail;
      })
    );
    const pending = store.loadConversations();
    fixture.detectChanges();
    const element = fixture.nativeElement as HTMLElement;
    expect(element.querySelector('[role="status"]')?.textContent).toContain('Loading conversations');
    expect(element.querySelector('.empty-list')).toBeNull();
    reject(new Error('Search failed'));
    await pending;
    fixture.detectChanges();
    expect(element.querySelector('[role="alert"]')?.textContent).toContain('Search failed');
    expect(element.querySelector('.empty-list')).toBeNull();
    const retry = [...element.querySelectorAll('button')].find((b) => b.textContent?.includes('Retry search'))!;
    retry.click();
    await vi.waitFor(() => expect(store.conversationsLoading()).toBe(false));
    fixture.detectChanges();
    expect(element.querySelector('[role="alert"]')).toBeNull();
    expect(element.querySelector('.empty-list')?.textContent).toContain('No conversations yet');
  });

  it('cancels a debounced search when the panel is destroyed', async () => {
    const { api, store, fixture } = setup();
    vi.useFakeTimers();
    try {
      fixture.detectChanges();
      const input = (fixture.nativeElement as HTMLElement).querySelector<HTMLInputElement>('input[type="search"]')!;
      input.value = 'new search';
      input.dispatchEvent(new Event('input'));
      expect(store.conversationQuery()).toBe('new search');
      fixture.destroy();
      await vi.advanceTimersByTimeAsync(300);
      expect(api.listConversations).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  function renameEditor() {
    const state = setup();
    const item: Conversation = {
      id: 'rename',
      title: 'Original',
      projectId: null,
      pinnedNote: '',
      temporary: false,
      messageCount: 0,
      createdAtMs: 1,
      updatedAtMs: 1,
    };
    state.store.conversations.set([item]);
    state.fixture.detectChanges();
    const element = state.fixture.nativeElement as HTMLElement;
    element.querySelector<HTMLButtonElement>('[aria-label="Rename"]')!.click();
    state.fixture.detectChanges();
    const input = element.querySelector<HTMLInputElement>('[aria-label="Conversation title"]')!;
    const type = (text: string) => {
      input.value = text;
      input.dispatchEvent(new Event('input'));
    };
    return { ...state, element, input, type };
  }

  it('retains a failed rename draft and retries with Enter', async () => {
    const { store, fixture, element, input, type } = renameEditor();
    const rename = vi.spyOn(store, 'rename').mockResolvedValueOnce(false).mockResolvedValueOnce(true);
    type('Retry title');
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter' }));
    await fixture.whenStable();
    fixture.detectChanges();
    expect(element.querySelector('[role="alert"]')?.textContent).toContain('Press Enter to retry');
    expect(input.value).toBe('Retry title');
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter' }));
    await fixture.whenStable();
    fixture.detectChanges();
    expect(rename).toHaveBeenCalledTimes(2);
    expect(element.querySelector('[aria-label="Conversation title"]')).toBeNull();
  });

  it('coalesces Enter/blur and keeps edits made while a rename is pending', async () => {
    const { store, fixture, element, input, type } = renameEditor();
    let finish!: (saved: boolean) => void;
    const rename = vi.spyOn(store, 'rename').mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      })
    );
    type('Submitted');
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter' }));
    input.dispatchEvent(new Event('blur'));
    expect(rename).toHaveBeenCalledTimes(1);
    type('New draft');
    finish(true);
    await fixture.whenStable();
    fixture.detectChanges();
    expect(element.querySelector('[aria-label="Conversation title"]')).not.toBeNull();
    expect(input.value).toBe('New draft');
    expect(element.querySelector('[role="status"]')).toBeNull();
  });

  it('does not close a reopened editor when an old rename completes', async () => {
    const { store, fixture, element, input, type } = renameEditor();
    let finish!: (saved: boolean) => void;
    vi.spyOn(store, 'rename').mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      })
    );
    type('Submitted');
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter' }));
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
    fixture.detectChanges();
    element.querySelector<HTMLButtonElement>('[aria-label="Rename"]')!.click();
    fixture.detectChanges();
    finish(true);
    await fixture.whenStable();
    fixture.detectChanges();
    expect(element.querySelector('[aria-label="Conversation title"]')).not.toBeNull();
  });

  it('rejects an empty title without discarding the editor', () => {
    const { store, fixture, element, input, type } = renameEditor();
    const rename = vi.spyOn(store, 'rename');
    type('  ');
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter' }));
    fixture.detectChanges();
    expect(rename).not.toHaveBeenCalled();
    expect(element.querySelector('[role="alert"]')?.textContent).toContain('Enter a conversation title');
    expect(element.querySelector('[aria-label="Conversation title"]')).not.toBeNull();
  });

  it.each(['failure', 'switched', 'success'])(
    'navigates after deletion only when the current chat was closed: %s',
    async (outcome) => {
      const { api, store, fixture } = setup();
      const item: Conversation = {
        id: 'old',
        title: 'Old chat',
        projectId: null,
        pinnedNote: '',
        temporary: false,
        messageCount: 0,
        createdAtMs: 1,
        updatedAtMs: 1,
      };
      store.conversation.set(item);
      store.conversations.set([item]);
      const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
      let resolve!: () => void;
      let reject!: (reason: unknown) => void;
      api.deleteConversation.mockReturnValueOnce(
        new Promise<undefined>((yes, no) => {
          resolve = () => yes(undefined);
          reject = no;
        })
      );
      fixture.detectChanges();
      const element = fixture.nativeElement as HTMLElement;
      element.querySelector<HTMLButtonElement>('button[aria-label="Delete"]')!.click();
      fixture.detectChanges();
      element.querySelector<HTMLButtonElement>('.confirm .danger')!.click();
      if (outcome === 'switched') {
        store.startNew();
        store.conversation.set({ ...item, id: 'new', title: 'New chat' });
      }
      if (outcome === 'failure') reject(new Error('Delete failed'));
      else resolve();
      await vi.waitFor(() => expect(store.deletingIds()).toEqual([]));
      if (outcome === 'success') {
        expect(navigate).toHaveBeenCalledWith([], { queryParams: { c: null } });
        expect(store.conversation()).toBeNull();
      } else {
        expect(navigate).not.toHaveBeenCalled();
        expect(store.conversation()?.id).toBe(outcome === 'failure' ? 'old' : 'new');
      }
    }
  );
});

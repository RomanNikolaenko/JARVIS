import { TestBed } from '@angular/core/testing';
import { provideRouter, Router } from '@angular/router';
import type { Conversation } from '@jarvis/shared-types';

import { JarvisApi } from '../../core/ipc/jarvis-api';
import { ChatStore } from '../../core/state/chat.store';
import { MemoryStore } from '../../core/state/memory.store';
import { Chat } from './chat';

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

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import type { Fact } from '@jarvis/shared-types';

import { JarvisApi } from '../../../core/ipc/jarvis-api';
import { MemoryStore } from '../../../core/state/memory.store';
import { FactsPanel } from './facts-panel';

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((complete) => {
    resolve = complete;
  });
  return { promise, resolve };
}

function fact(id: string): Fact {
  return {
    id,
    content: `Fact ${id}`,
    projectId: 'p1',
    pinned: false,
    source: 'conversation',
    sourceConversationId: `chat-${id}`,
    createdAtMs: 1,
    updatedAtMs: 1,
  };
}

describe('FactsPanel editor', () => {
  async function render() {
    TestBed.configureTestingModule({ providers: [provideRouter([]), { provide: JarvisApi, useValue: {} }] });
    const store = TestBed.inject(MemoryStore);
    store.facts.set([fact('a'), fact('b')]);
    store.projects.set(
      ['p1', 'p2'].map((id) => ({
        id,
        name: id,
        description: '',
        instructions: '',
        factCount: 1,
        conversationCount: 1,
        createdAtMs: 1,
        updatedAtMs: 1,
      }))
    );
    const save = vi.spyOn(store, 'saveFact');
    const remove = vi.spyOn(store, 'deleteFact');
    const fixture = TestBed.createComponent(FactsPanel);
    await fixture.whenStable();
    fixture.detectChanges();
    const element = fixture.nativeElement as HTMLElement;
    const content = () => element.querySelector<HTMLTextAreaElement>('.form textarea')!;
    const project = () => element.querySelector<HTMLSelectElement>('.form select')!;
    const pinned = () => element.querySelector<HTMLInputElement>('.form input[type="checkbox"]')!;
    const button = (label: string) =>
      [...element.querySelectorAll<HTMLButtonElement>('.form button')].find((item) => item.textContent?.trim() === label)!;
    const edit = (index: number) => {
      element.querySelectorAll<HTMLButtonElement>('[aria-label="Edit"]')[index].click();
      fixture.detectChanges();
    };
    const type = (value: string) => {
      content().value = value;
      content().dispatchEvent(new Event('input'));
      fixture.detectChanges();
    };
    return { store, save, remove, fixture, element, content, project, pinned, button, edit, type };
  }

  it('clears an acknowledged unchanged edit and keeps its original conversation link', async () => {
    const { save, fixture, content, button, edit, type } = await render();
    save.mockResolvedValueOnce({ ...fact('a'), content: 'Edited fact' });
    edit(0);
    type('Edited fact');
    button('Save changes').click();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(save).toHaveBeenCalledExactlyOnceWith({
      id: 'a',
      content: 'Edited fact',
      projectId: 'p1',
      pinned: false,
      sourceConversationId: 'chat-a',
    });
    expect(content().value).toBe('');
    expect(button('Remember')).toBeDefined();
  });

  it('preserves edits made during creation and binds the next explicit save to the created fact', async () => {
    const { store, save, fixture, content, button, type } = await render();
    const pending = deferred<Fact | null>();
    save.mockReturnValueOnce(pending.promise).mockResolvedValueOnce({ ...fact('created'), content: 'Latest draft' });
    type('Submitted draft');
    button('Remember').click();
    type('Latest draft');
    const created = { ...fact('created'), content: 'Submitted draft' };
    store.facts.update((items) => [created, ...items]);
    pending.resolve(created);
    await fixture.whenStable();
    fixture.detectChanges();
    expect(content().value).toBe('Latest draft');
    button('Save changes').click();
    await fixture.whenStable();
    expect(save.mock.calls.map(([input]) => input.id)).toEqual([null, 'created']);
    expect(save.mock.calls[1][0].content).toBe('Latest draft');
  });

  it.each(['content', 'project', 'pinned'] as const)(
    'keeps an edit away and back in %s while a save is pending',
    async (field) => {
      const { save, fixture, content, project, pinned, button, edit, type } = await render();
      const pending = deferred<Fact | null>();
      save.mockReturnValueOnce(pending.promise);
      edit(0);
      button('Save changes').click();
      if (field === 'content') {
        type('Other text');
        type('Fact a');
      } else if (field === 'project') {
        for (const id of ['p2', 'p1']) {
          project().value = id;
          project().dispatchEvent(new Event('change'));
        }
      } else {
        for (const value of [true, false]) {
          pinned().checked = value;
          pinned().dispatchEvent(new Event('change'));
        }
      }
      pending.resolve(fact('a'));
      await fixture.whenStable();
      fixture.detectChanges();
      expect(content().value).toBe('Fact a');
      expect(button('Save changes')).toBeDefined();
      expect(save).toHaveBeenCalledOnce();
    }
  );

  it.each(['other', 'reopen'] as const)('does not clear a replacement editor after a save completes: %s', async (action) => {
    const { save, fixture, content, button, edit, type } = await render();
    const pending = deferred<Fact | null>();
    save.mockReturnValueOnce(pending.promise);
    edit(0);
    button('Save changes').click();
    if (action === 'reopen') {
      button('Cancel').click();
      fixture.detectChanges();
      edit(0);
    } else edit(1);
    type('Replacement editor draft');
    pending.resolve(fact('a'));
    await fixture.whenStable();
    fixture.detectChanges();
    expect(content().value).toBe('Replacement editor draft');
    expect(button('Save changes').disabled).toBe(false);
  });

  it('blocks a stale save listener before a replacement editor is rendered', async () => {
    const { save, element, button, edit } = await render();
    edit(0);
    const oldSave = button('Save changes');
    button('Cancel').click();
    element.querySelectorAll<HTMLButtonElement>('[aria-label="Edit"]')[1].click();
    oldSave.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    await Promise.resolve();
    expect(save).not.toHaveBeenCalled();
  });

  it('coalesces forced duplicate clicks while a native save is pending', async () => {
    const { save, fixture, button, type } = await render();
    const pending = deferred<Fact | null>();
    save.mockReturnValueOnce(pending.promise);
    type('One fact');
    const remember = button('Remember');
    remember.click();
    remember.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    expect(save).toHaveBeenCalledOnce();
    pending.resolve(null);
    await fixture.whenStable();
  });

  it('blocks conflicting pin and delete actions while the edited fact is saving', async () => {
    const { store, save, remove, fixture, element, button, edit } = await render();
    const pending = deferred<Fact | null>();
    const pin = vi.spyOn(store, 'togglePinned').mockResolvedValue(null);
    save.mockReturnValueOnce(pending.promise);
    edit(0);
    button('Save changes').click();
    fixture.detectChanges();
    const pinButton = element.querySelector<HTMLButtonElement>('[aria-label="Pin"]')!;
    expect(pinButton.disabled).toBe(true);
    pinButton.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    element.querySelector<HTMLButtonElement>('[aria-label="Forget"]')!.click();
    fixture.detectChanges();
    const forget = element.querySelector<HTMLButtonElement>('.confirm .danger')!;
    expect(forget.disabled).toBe(true);
    forget.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    expect(pin).not.toHaveBeenCalled();
    expect(remove).not.toHaveBeenCalled();
    pending.resolve(fact('a'));
    await fixture.whenStable();
  });

  it('keeps the fact draft when deletion fails', async () => {
    const { remove, fixture, element, content, edit, type } = await render();
    remove.mockResolvedValueOnce(false);
    edit(0);
    type('Unsaved edit');
    element.querySelector<HTMLButtonElement>('[aria-label="Forget"]')!.click();
    fixture.detectChanges();
    element.querySelector<HTMLButtonElement>('.confirm .danger')!.click();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(content().value).toBe('Unsaved edit');
    expect(remove).toHaveBeenCalledExactlyOnceWith('a');
  });

  it('does not clear another editor when a deletion finishes', async () => {
    const { remove, fixture, element, content, edit, type } = await render();
    const pending = deferred<boolean>();
    remove.mockReturnValueOnce(pending.promise);
    edit(0);
    element.querySelector<HTMLButtonElement>('[aria-label="Forget"]')!.click();
    fixture.detectChanges();
    element.querySelector<HTMLButtonElement>('.confirm .danger')!.click();
    edit(0); // The remaining Edit button belongs to b while a's confirmation is open.
    type('Draft for fact b');
    pending.resolve(true);
    await fixture.whenStable();
    fixture.detectChanges();
    expect(content().value).toBe('Draft for fact b');
  });

  it('does not affect a newly mounted form when a destroyed form save completes', async () => {
    const { save, fixture, button, type } = await render();
    const pending = deferred<Fact | null>();
    save.mockReturnValueOnce(pending.promise);
    type('Old submitted fact');
    button('Remember').click();
    fixture.destroy();
    const next = TestBed.createComponent(FactsPanel);
    await next.whenStable();
    next.detectChanges();
    const input = (next.nativeElement as HTMLElement).querySelector<HTMLTextAreaElement>('.form textarea')!;
    input.value = 'New form draft';
    input.dispatchEvent(new Event('input'));
    pending.resolve(fact('created'));
    await next.whenStable();
    next.detectChanges();
    expect(input.value).toBe('New form draft');
    expect(save).toHaveBeenCalledOnce();
  });

  it('keeps edits made during a confirmed deletion available as a new fact', async () => {
    const { store, save, remove, fixture, element, content, button, edit, type } = await render();
    const pending = deferred<boolean>();
    remove.mockReturnValueOnce(pending.promise);
    save.mockResolvedValueOnce(fact('new-fact'));
    edit(0);
    element.querySelector<HTMLButtonElement>('[aria-label="Forget"]')!.click();
    fixture.detectChanges();
    element.querySelector<HTMLButtonElement>('.confirm .danger')!.click();
    type('New fact draft after deleting the old one');
    store.facts.update((items) => items.filter((item) => item.id !== 'a'));
    pending.resolve(true);
    await fixture.whenStable();
    fixture.detectChanges();
    expect(content().value).toBe('New fact draft after deleting the old one');
    button('Remember').click();
    await fixture.whenStable();
    expect(save).toHaveBeenCalledExactlyOnceWith(
      expect.objectContaining({ id: null, content: 'New fact draft after deleting the old one' })
    );
  });
});

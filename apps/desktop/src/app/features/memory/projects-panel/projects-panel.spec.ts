import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import type { Project, ProjectInput } from '@jarvis/shared-types';

import { ChatStore } from '../../../core/state/chat.store';
import { MemoryStore } from '../../../core/state/memory.store';
import { ProjectsPanel } from './projects-panel';

function project(id: string, name = `Project ${id}`): Project {
  return {
    id,
    name,
    description: `Description ${id}`,
    instructions: `Instructions ${id}`,
    factCount: 0,
    conversationCount: 0,
    createdAtMs: 1,
    updatedAtMs: 1,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((complete) => {
    resolve = complete;
  });
  return { promise, resolve };
}

type Field = 'name' | 'description' | 'instructions';

describe('ProjectsPanel save ownership', () => {
  function render() {
    const store = {
      busy: signal(false),
      projects: signal([project('one'), project('two')]),
      saveProject: vi.fn<(input: ProjectInput) => Promise<Project | null>>(async (input) => ({
        ...project(input.id ?? 'created'),
        ...input,
        id: input.id ?? 'created',
      })),
      deleteProject: vi.fn(async () => undefined),
    };
    TestBed.configureTestingModule({
      providers: [
        provideRouter([]),
        { provide: MemoryStore, useValue: store },
        { provide: ChatStore, useValue: { startNew: vi.fn() } },
      ],
    });
    const fixture = TestBed.createComponent(ProjectsPanel);
    fixture.detectChanges();
    const element = fixture.nativeElement as HTMLElement;
    const button = (label: string) =>
      [...element.querySelectorAll<HTMLButtonElement>('button')].find((item) => item.textContent?.trim() === label)!;
    const field = (name: Field) => {
      if (name === 'name') return element.querySelector<HTMLInputElement>('.form input')!;
      return element.querySelectorAll<HTMLTextAreaElement>('.form textarea')[name === 'description' ? 0 : 1];
    };
    const type = (name: Field, value: string) => {
      field(name).value = value;
      field(name).dispatchEvent(new Event('input'));
      fixture.detectChanges();
    };
    const edit = (index: number, detectChanges = true) => {
      element.querySelectorAll<HTMLButtonElement>('.row [aria-label="Edit"]')[index].click();
      if (detectChanges) fixture.detectChanges();
    };
    const create = () => {
      button('New project').click();
      fixture.detectChanges();
    };
    return { store, fixture, element, button, field, type, edit, create };
  }

  it('closes the original form after a confirmed save and sends trimmed fields', async () => {
    const { store, fixture, element, button, type, create } = render();
    create();
    type('name', '  My project  ');
    type('description', '  Description  ');
    type('instructions', '  Answer concisely  ');
    button('Save project').click();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(store.saveProject).toHaveBeenCalledExactlyOnceWith({
      id: null,
      name: 'My project',
      description: 'Description',
      instructions: 'Answer concisely',
    });
    expect(element.querySelector('.form')).toBeNull();
  });

  it.each(['name', 'description', 'instructions'] as const)(
    'keeps a newer %s revision, including an edit back to the submitted value',
    async (name) => {
      const { store, fixture, element, button, field, type, edit } = render();
      const pending = deferred<Project | null>();
      store.saveProject.mockReturnValueOnce(pending.promise);
      edit(0);
      const submitted = field(name).value;
      button('Save project').click();
      type(name, 'Newer draft');
      type(name, submitted);
      pending.resolve(project('one'));
      await fixture.whenStable();
      fixture.detectChanges();
      expect(element.querySelector('.form')).not.toBeNull();
      expect(field(name).value).toBe(submitted);
      expect(button('Save project').disabled).toBe(false);
      expect(store.saveProject).toHaveBeenCalledOnce();
    }
  );

  it('binds newer edits to the newly created ID so an explicit follow-up save updates it', async () => {
    const { store, fixture, element, button, field, type, create } = render();
    const pending = deferred<Project | null>();
    store.saveProject.mockReturnValueOnce(pending.promise);
    create();
    type('name', 'First name');
    button('Save project').click();
    type('name', 'Latest name');
    type('description', 'Newer description');
    pending.resolve(project('saved-id', 'First name'));
    await fixture.whenStable();
    fixture.detectChanges();
    expect(field('name').value).toBe('Latest name');
    expect(field('description').value).toBe('Newer description');
    expect(element.textContent).toContain('Edit project');
    expect(store.saveProject).toHaveBeenCalledOnce();
    button('Save project').click();
    await fixture.whenStable();
    expect(store.saveProject).toHaveBeenLastCalledWith({
      id: 'saved-id',
      name: 'Latest name',
      description: 'Newer description',
      instructions: '',
    });
    expect(store.saveProject).toHaveBeenCalledTimes(2);
  });

  it('preserves another project editor when an earlier save finishes', async () => {
    const { store, fixture, element, button, field, type, edit } = render();
    const pending = deferred<Project | null>();
    store.saveProject.mockReturnValueOnce(pending.promise);
    edit(0);
    button('Save project').click();
    edit(1);
    type('instructions', 'New instructions for the second project');
    pending.resolve(project('one'));
    await fixture.whenStable();
    fixture.detectChanges();
    expect(element.querySelector('.form')).not.toBeNull();
    expect(field('name').value).toBe('Project two');
    expect(field('instructions').value).toBe('New instructions for the second project');
    expect(button('Save project').disabled).toBe(false);
  });

  it('keeps a cancelled and reopened editor for the same project', async () => {
    const { store, fixture, element, button, field, type, edit } = render();
    const pending = deferred<Project | null>();
    store.saveProject.mockReturnValueOnce(pending.promise);
    edit(0);
    button('Save project').click();
    button('Cancel').click();
    fixture.detectChanges();
    edit(0);
    type('name', 'Reopened project draft');
    pending.resolve(project('one'));
    await fixture.whenStable();
    fixture.detectChanges();
    expect(element.querySelector('.form')).not.toBeNull();
    expect(field('name').value).toBe('Reopened project draft');
  });

  it('does not rebind a different new-project editor to the earlier created ID', async () => {
    const { store, fixture, button, field, type, create } = render();
    const pending = deferred<Project | null>();
    store.saveProject.mockReturnValueOnce(pending.promise);
    create();
    type('name', 'Original new project');
    button('Save project').click();
    create();
    type('name', 'Different new project');
    pending.resolve(project('original-created'));
    await fixture.whenStable();
    fixture.detectChanges();
    expect(field('name').value).toBe('Different new project');
    button('Save project').click();
    await fixture.whenStable();
    expect(store.saveProject).toHaveBeenLastCalledWith(expect.objectContaining({ id: null, name: 'Different new project' }));
  });

  it('does not alter a replacement component after the original component is destroyed', async () => {
    const { store, fixture, button, edit } = render();
    const pending = deferred<Project | null>();
    store.saveProject.mockReturnValueOnce(pending.promise);
    edit(0);
    button('Save project').click();
    fixture.destroy();
    const replacement = TestBed.createComponent(ProjectsPanel);
    replacement.detectChanges();
    const element = replacement.nativeElement as HTMLElement;
    [...element.querySelectorAll<HTMLButtonElement>('button')]
      .find((item) => item.textContent?.trim() === 'New project')!
      .click();
    replacement.detectChanges();
    const input = element.querySelector<HTMLInputElement>('.form input')!;
    input.value = 'Replacement draft';
    input.dispatchEvent(new Event('input'));
    pending.resolve(project('one'));
    await replacement.whenStable();
    replacement.detectChanges();
    expect(input.value).toBe('Replacement draft');
    expect(element.querySelector('.form')).not.toBeNull();
    expect(store.saveProject).toHaveBeenCalledOnce();
  });

  it('coalesces forced duplicate Save actions even when the store is not marked busy', async () => {
    const { store, fixture, button, edit } = render();
    const pending = deferred<Project | null>();
    store.saveProject.mockReturnValueOnce(pending.promise);
    edit(0);
    const save = button('Save project');
    save.click();
    save.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    expect(store.busy()).toBe(false);
    expect(store.saveProject).toHaveBeenCalledOnce();
    pending.resolve(project('one'));
    await fixture.whenStable();
  });

  it('blocks a forced project deletion while its save is still pending', async () => {
    const { store, fixture, element, button, edit } = render();
    const pending = deferred<Project | null>();
    store.saveProject.mockReturnValueOnce(pending.promise);
    edit(0);
    button('Save project').click();
    element.querySelector<HTMLButtonElement>('.row [aria-label="Delete"]')!.click();
    fixture.detectChanges();
    const remove = element.querySelector<HTMLButtonElement>('.confirm .danger')!;
    expect(remove.disabled).toBe(true);
    remove.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    expect(store.deleteProject).not.toHaveBeenCalled();
    pending.resolve(project('one'));
    await fixture.whenStable();
    fixture.detectChanges();
    expect(remove.disabled).toBe(false);
    remove.click();
    await fixture.whenStable();
    expect(store.deleteProject).toHaveBeenCalledExactlyOnceWith('one');
  });

  it('rejects a stale Save action for a different editor before Angular renders it', async () => {
    const { store, fixture, button, field, edit } = render();
    edit(0);
    const oldSave = button('Save project');
    edit(1, false);
    oldSave.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    await Promise.resolve();
    expect(store.saveProject).not.toHaveBeenCalled();
    fixture.detectChanges();
    expect(field('name').value).toBe('Project two');
  });

  it('refuses a stale Save action after the form closes', async () => {
    const { store, button, edit } = render();
    edit(0);
    const oldSave = button('Save project');
    button('Cancel').click();
    oldSave.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    await Promise.resolve();
    expect(store.saveProject).not.toHaveBeenCalled();
  });

  it.each([
    ['name', 'x'.repeat(101)],
    ['description', 'x'.repeat(2001)],
    ['instructions', 'x'.repeat(8001)],
  ] as const)('refuses a forced oversized %s field', async (name, value) => {
    const { store, button, type, edit } = render();
    edit(0);
    type(name, value);
    const save = button('Save project');
    expect(save.disabled).toBe(true);
    save.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    await Promise.resolve();
    expect(store.saveProject).not.toHaveBeenCalled();
  });
});

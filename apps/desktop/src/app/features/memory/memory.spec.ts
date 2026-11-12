import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import type { AcceptFactSuggestion, FactInput, FactSuggestion, MemoryStatus, Settings } from '@jarvis/shared-types';

import { JarvisApi } from '../../core/ipc/jarvis-api';
import { IpcError } from '../../core/ipc/ipc-error';
import { MemoryStore } from '../../core/state/memory.store';
import { SettingsStore } from '../../core/state/settings.store';
import { SETTINGS } from '../../testing/settings.fixture';
import { MemoryPage } from './memory';

const STATUS: MemoryStatus = {
  available: true,
  error: null,
  savingEnabled: true,
  databasePath: '/data/memory/jarvis.db',
  sizeBytes: 8192,
  schemaVersion: 1,
  counts: { conversations: 2, messages: 6, facts: 1, projects: 0, episodes: 4 },
  embedding: { state: 'unavailable', model: null, indexed: 0, pending: 0, detail: 'text only' },
  encryption: 'off',
};

describe('MemoryPage', () => {
  const api = {
    connected: true,
    getMemoryStatus: vi.fn(async () => STATUS),
    listFacts: vi.fn(async () => [
      {
        id: 'f1',
        content: 'The user prefers short answers.',
        projectId: null,
        pinned: true,
        source: 'user',
        sourceConversationId: null,
        createdAtMs: 1,
        updatedAtMs: 1,
      },
    ]),
    listProjects: vi.fn(async () => []),
    saveFact: vi.fn(async (input: FactInput) => ({
      ...input,
      id: 'saved-fact',
      source: 'user' as const,
      createdAtMs: 1,
      updatedAtMs: 1,
    })),
    listEpisodes: vi.fn(async () => []),
    listAiModels: vi.fn(async () => []),
    getModelRoutes: vi.fn(async () => []),
    clearMemory: vi.fn(async () => undefined),
    searchMemory: vi.fn(async () => []),
    listFactSuggestions: vi.fn(async (): Promise<FactSuggestion[]> => []),
    acceptFactSuggestion: vi.fn(async (input: AcceptFactSuggestion) => ({
      id: 'f2',
      content: input.content,
      projectId: input.projectId,
      pinned: input.pinned,
      source: 'conversation',
      sourceConversationId: 'c1',
      createdAtMs: 2,
      updatedAtMs: 2,
    })),
    rejectFactSuggestion: vi.fn(async () => undefined),
    updateSettings: vi.fn(async (settings: Settings) => settings),
    prepareMemoryEncryption: vi.fn(async () => ({ code: '111111-222222-333333-444444-555555-666666-777777-888888' })),
    encryptMemory: vi.fn(async (_confirmation: string) => undefined),
    unlockMemory: vi.fn(async (_code: string) => undefined),
  };

  afterEach(() => {
    api.getMemoryStatus.mockResolvedValue(STATUS);
    TestBed.resetTestingModule();
  });

  async function openData(fixture: Awaited<ReturnType<typeof render>>['fixture'], element: HTMLElement) {
    button(element, 'Data & settings').click();
    await fixture.whenStable();
    fixture.detectChanges();
  }

  function encryptionRow(element: HTMLElement): string {
    const row = [...element.querySelectorAll('.facts-list > div')].find(
      (d) => d.querySelector('dt')?.textContent === 'Encryption'
    );
    return row?.querySelector('dd')?.textContent?.replace(/\s+/g, ' ').trim() ?? '';
  }

  function suggestion(id: string, content: string): FactSuggestion {
    return { id, content, projectId: null, sourceConversationId: 'c1', sourceConversationTitle: 'Morning chat', createdAtMs: 1 };
  }

  async function render() {
    TestBed.configureTestingModule({
      imports: [MemoryPage],
      providers: [provideRouter([]), { provide: JarvisApi, useValue: api }],
    });
    TestBed.inject(SettingsStore).settings.set(SETTINGS);
    const fixture = TestBed.createComponent(MemoryPage);
    await fixture.whenStable();
    fixture.detectChanges();
    return { fixture, element: fixture.nativeElement as HTMLElement };
  }

  function button(element: HTMLElement, text: string): HTMLButtonElement {
    const found = [...element.querySelectorAll('button')].find((b) => b.textContent?.includes(text));
    if (!found) {
      throw new Error(`no button "${text}"`);
    }
    return found;
  }

  it('shows counts and facts', async () => {
    const { element } = await render();
    expect(element.querySelector('.subtitle')?.textContent).toContain('2 conversations');
    expect(element.textContent).toContain('The user prefers short answers.');
    expect(element.querySelector('.chip.accent')?.textContent).toContain('pinned');
  });

  it('offers a read-only reload after a confirmed save refresh fails without repeating the save', async () => {
    const { fixture, element } = await render();
    const store = TestBed.inject(MemoryStore);
    api.saveFact.mockClear();
    api.listFacts.mockRejectedValueOnce(new Error('List read failed'));
    const saved = await store.saveFact({
      id: null,
      content: 'Confirmed saved fact',
      projectId: null,
      pinned: false,
      sourceConversationId: null,
    });
    expect(saved?.id).toBe('saved-fact');
    await vi.waitFor(() => expect(store.refreshError()?.message).toBe('List read failed'));
    fixture.detectChanges();
    expect(element.querySelector('.refresh-error')?.textContent).toContain('Changes were saved');
    expect(element.querySelector('.refresh-error')?.textContent).toContain('List read failed');
    expect(store.error()).toBeNull();
    expect(store.busy()).toBe(false);
    api.listFacts.mockResolvedValueOnce([
      {
        id: 'saved-fact',
        content: 'Confirmed saved fact',
        projectId: null,
        pinned: false,
        source: 'user',
        sourceConversationId: null,
        createdAtMs: 1,
        updatedAtMs: 1,
      },
    ]);
    button(element, 'Reload memory').click();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(element.querySelector('.refresh-error')).toBeNull();
    expect(element.textContent).toContain('Confirmed saved fact');
    expect(api.saveFact).toHaveBeenCalledOnce();
  });

  it('does not start a reload while a foreground operation is busy', async () => {
    const { fixture, element } = await render();
    const store = TestBed.inject(MemoryStore);
    store.refreshError.set(new IpcError('timeout', 'Lists did not respond'));
    store.busy.set(true);
    const reload = vi.spyOn(store, 'loadAll').mockResolvedValue();
    fixture.detectChanges();
    const retry = button(element, 'Reload memory');
    expect(retry.disabled).toBe(true);
    retry.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    expect(reload).not.toHaveBeenCalled();
    store.busy.set(false);
    fixture.detectChanges();
    retry.click();
    await fixture.whenStable();
    expect(reload).toHaveBeenCalledOnce();
  });

  it('shows suggested facts for review and accepts or rejects them', async () => {
    api.listFactSuggestions.mockResolvedValue([
      suggestion('s1', 'Користувач працює вчителем.'),
      suggestion('s2', 'Користувач має кота.'),
    ]);
    const { fixture, element } = await render();
    const section = element.querySelector('.suggestions');
    expect(section?.textContent).toContain('Suggested (2)');
    expect(section?.textContent).toContain('Morning chat');
    const text = section!.querySelector<HTMLTextAreaElement>('textarea[aria-label="Suggested fact"]')!;
    text.value = 'Користувач — учитель історії.';
    text.dispatchEvent(new Event('input'));
    button(section as HTMLElement, 'Accept').click();
    await fixture.whenStable();
    expect(api.acceptFactSuggestion).toHaveBeenCalledWith({
      id: 's1',
      content: 'Користувач — учитель історії.',
      pinned: false,
      projectId: null,
    });
    fixture.detectChanges();
    button(element.querySelector('.suggestions') as HTMLElement, 'Reject').click();
    await fixture.whenStable();
    expect(api.rejectFactSuggestion).toHaveBeenCalledWith('s2');
    api.listFactSuggestions.mockResolvedValue([]);
  });

  it('has no suggestions section when nothing waits for review', async () => {
    const { element } = await render();
    expect(element.querySelector('.suggestions')).toBeNull();
  });

  it('saves the setting that suggests facts from conversations', async () => {
    const { fixture, element } = await render();
    button(element, 'Data & settings').click();
    await fixture.whenStable();
    fixture.detectChanges();
    const label = [...element.querySelectorAll('label.check')].find((l) =>
      l.textContent?.includes('Suggest facts from conversations')
    );
    const checkbox = label!.querySelector<HTMLInputElement>('input[type="checkbox"]')!;
    expect(checkbox.checked).toBe(false);
    checkbox.checked = true;
    checkbox.dispatchEvent(new Event('change'));
    fixture.detectChanges();
    const save = [...element.querySelectorAll('button')].find((b) => b.textContent?.trim() === 'Save')!;
    save.click();
    await fixture.whenStable();
    expect(api.updateSettings).toHaveBeenCalledWith(
      expect.objectContaining({ memory: expect.objectContaining({ suggestFacts: true }) })
    );
  });

  it('deletes everything only after the confirmation word is typed', async () => {
    const { fixture, element } = await render();
    button(element, 'Data & settings').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const clear = button(element, 'Delete all memory');
    expect(clear.disabled).toBe(true);
    const input = element.querySelector<HTMLInputElement>('input[aria-label="Confirmation"]');
    input!.value = 'DELETE';
    input!.dispatchEvent(new Event('input'));
    fixture.detectChanges();
    expect(clear.disabled).toBe(false);

    clear.click();
    await fixture.whenStable();
    expect(api.clearMemory).toHaveBeenCalledWith('DELETE ALL MEMORY');
  });

  it.each([
    ['off', 'Memory is not encrypted'],
    ['on', 'Encrypted (key in the system keychain)'],
    ['locked', 'Locked'],
    ['unsupported', 'Encryption needs the system keychain, which is not available'],
  ] as const)('shows the encryption state %s', async (encryption, text) => {
    api.getMemoryStatus.mockResolvedValue({ ...STATUS, available: encryption !== 'locked', encryption });
    const { fixture, element } = await render();
    await openData(fixture, element);
    expect(encryptionRow(element)).toContain(text);
  });

  it('encrypts only after the last group of the recovery code is typed', async () => {
    const { fixture, element } = await render();
    await openData(fixture, element);
    button(element, 'Encrypt memory…').click();
    await fixture.whenStable();
    fixture.detectChanges();
    const dialog = element.querySelector<HTMLElement>('[role="dialog"]')!;
    expect(dialog).not.toBeNull();
    const groups = [...dialog.querySelectorAll('.code-group')].map((g) => g.textContent?.trim());
    expect(groups).toEqual(['111111', '222222', '333333', '444444', '555555', '666666', '777777', '888888']);
    expect(dialog.textContent).toContain('Without this code and without the keychain entry');
    const encrypt = button(dialog, 'Encrypt');
    expect(encrypt.disabled).toBe(true);
    const input = dialog.querySelector<HTMLInputElement>('input[aria-label="Last group of the recovery code"]')!;
    input.value = '777777';
    input.dispatchEvent(new Event('input'));
    fixture.detectChanges();
    expect(encrypt.disabled).toBe(true);
    input.value = '888888';
    input.dispatchEvent(new Event('input'));
    fixture.detectChanges();
    expect(encrypt.disabled).toBe(false);
    api.getMemoryStatus.mockResolvedValue({ ...STATUS, encryption: 'on' });
    encrypt.click();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(api.encryptMemory).toHaveBeenCalledWith('888888');
    expect(element.querySelector('[role="dialog"]')).toBeNull();
    expect(encryptionRow(element)).toContain('Encrypted (key in the system keychain)');
  });

  it('unlocks locked memory with the recovery code and shows a wrong code', async () => {
    api.getMemoryStatus.mockResolvedValue({ ...STATUS, available: false, error: 'locked', encryption: 'locked' });
    api.unlockMemory.mockRejectedValueOnce({
      code: 'validation',
      message: 'This recovery code does not open the memory database',
    });
    const { fixture, element } = await render();
    const form = element.querySelector<HTMLElement>('.unlock')!;
    expect(form.textContent).toContain('Recovery code');
    const input = form.querySelector<HTMLInputElement>('input[aria-label="Recovery code"]')!;
    input.value = '123';
    input.dispatchEvent(new Event('input'));
    fixture.detectChanges();
    button(form, 'Unlock').click();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(api.unlockMemory).toHaveBeenCalledWith('123');
    expect(element.querySelector('.unlock')?.textContent).toContain('This recovery code does not open the memory database');

    api.getMemoryStatus.mockResolvedValue({ ...STATUS, encryption: 'on' });
    input.value = '111111-222222-333333-444444-555555-666666-777777-888888';
    input.dispatchEvent(new Event('input'));
    fixture.detectChanges();
    button(element.querySelector<HTMLElement>('.unlock')!, 'Unlock').click();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(api.unlockMemory).toHaveBeenLastCalledWith('111111-222222-333333-444444-555555-666666-777777-888888');
    expect(element.querySelector('.unlock')).toBeNull();
  });

  it('notes that an export of encrypted memory is a plain file', async () => {
    api.getMemoryStatus.mockResolvedValue({ ...STATUS, encryption: 'on' });
    const { fixture, element } = await render();
    await openData(fixture, element);
    expect(element.textContent).toContain('An export is a plain JSON file');
  });

  it('disables encryption without a keychain and says why', async () => {
    api.getMemoryStatus.mockResolvedValue({ ...STATUS, encryption: 'unsupported' });
    const { fixture, element } = await render();
    await openData(fixture, element);
    const encrypt = button(element, 'Encrypt memory…');
    expect(encrypt.disabled).toBe(true);
    expect(encrypt.title).toContain('system keychain');
  });

  it('renders search snippets with highlights and without extra spaces', async () => {
    const { fixture, element } = await render();
    TestBed.inject(MemoryStore).results.set([
      {
        kind: 'message',
        id: 'm1',
        conversationId: 'c1',
        conversationTitle: 'Atlas',
        role: 'user',
        snippet: 'Коли \u0002дедлайн\u0003 проєкту?',
        score: 1,
        matchedText: true,
        matchedSemantic: false,
        createdAtMs: 1,
      },
    ]);
    button(element, 'Search').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const text = element.querySelector('jv-search-panel .row-text');
    expect(text?.textContent).toBe('Коли дедлайн проєкту?');
    expect(text?.querySelector('.match')?.textContent).toBe('дедлайн');
  });

  it('clears results on draft changes and does not search with all filters disabled', async () => {
    const { fixture, element } = await render();
    button(element, 'Search').click();
    await fixture.whenStable();
    fixture.detectChanges();
    const store = TestBed.inject(MemoryStore);
    store.results.set([]);
    fixture.detectChanges();
    const input = element.querySelector<HTMLInputElement>('input[aria-label="Search text"]')!;
    input.value = 'atlas';
    input.dispatchEvent(new Event('input'));
    fixture.detectChanges();
    expect(store.results()).toBeNull();
    store.results.set([]);
    for (const checkbox of element.querySelectorAll<HTMLInputElement>('jv-search-panel input[type="checkbox"]')) {
      checkbox.checked = false;
      checkbox.dispatchEvent(new Event('change'));
    }
    fixture.detectChanges();
    expect(store.results()).toBeNull();
    api.searchMemory.mockClear();
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter' }));
    await fixture.whenStable();
    expect(api.searchMemory).not.toHaveBeenCalled();
  });
});

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import type { MemoryStatus } from '@jarvis/shared-types';

import { JarvisApi } from '../../core/ipc/jarvis-api';
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
    listEpisodes: vi.fn(async () => []),
    listAiModels: vi.fn(async () => []),
    getModelRoutes: vi.fn(async () => []),
    clearMemory: vi.fn(async () => undefined),
  };

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
    const found = [...element.querySelectorAll('button')].find((b) =>
      b.textContent?.includes(text),
    );
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
});

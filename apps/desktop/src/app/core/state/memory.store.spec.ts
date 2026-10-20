import { TestBed } from '@angular/core/testing';
import type { Episode, Fact, FactInput, MemoryStatus, SearchQuery } from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';
import { CLEAR_CONFIRMATION, EPISODE_PAGE, MemoryStore, snippetParts } from './memory.store';

function fact(id: string, pinned = false): Fact {
  return {
    id,
    content: `fact ${id}`,
    projectId: null,
    pinned,
    source: 'user',
    sourceConversationId: null,
    createdAtMs: 1,
    updatedAtMs: 1,
  };
}

function episode(at: number): Episode {
  return { id: `e${at}`, occurredAtMs: at, category: 'runtime', level: 'info', summary: 's' };
}

const STATUS: MemoryStatus = {
  available: true,
  error: null,
  savingEnabled: true,
  databasePath: '/data/memory/jarvis.db',
  sizeBytes: 4096,
  schemaVersion: 1,
  counts: { conversations: 1, messages: 2, facts: 1, projects: 0, episodes: 3 },
  embedding: { state: 'unavailable', model: null, indexed: 0, pending: 0, detail: null },
};

class FakeApi {
  facts: Fact[] = [fact('a')];
  saved: FactInput[] = [];
  searches: SearchQuery[] = [];
  clearedWith: string[] = [];
  episodeRequests: (number | null)[] = [];

  getMemoryStatus = vi.fn(async () => STATUS);
  listFacts = vi.fn(async () => this.facts);
  listProjects = vi.fn(async () => []);
  async listEpisodes(limit: number, beforeMs: number | null): Promise<Episode[]> {
    this.episodeRequests.push(beforeMs);
    const start = beforeMs ?? 1000;
    const count = beforeMs === null ? limit : 3;
    return Array.from({ length: count }, (_, i) => episode(start - i - 1));
  }
  async saveFact(input: FactInput): Promise<Fact> {
    this.saved.push(input);
    const saved = { ...fact(input.id ?? 'new', input.pinned), content: input.content };
    this.facts = [saved, ...this.facts.filter((f) => f.id !== saved.id)];
    return saved;
  }
  async deleteFact(id: string): Promise<void> {
    if (id === 'missing') {
      throw new IpcError('notFound', 'the fact does not exist');
    }
    this.facts = this.facts.filter((f) => f.id !== id);
  }
  async searchMemory(query: SearchQuery) {
    this.searches.push(query);
    return [];
  }
  async clearMemory(confirmation: string): Promise<void> {
    this.clearedWith.push(confirmation);
  }
  exportMemory = vi.fn(async () => null);
}

describe('MemoryStore', () => {
  let store: MemoryStore;
  let api: FakeApi;

  beforeEach(async () => {
    api = new FakeApi();
    TestBed.configureTestingModule({ providers: [{ provide: JarvisApi, useValue: api }] });
    store = TestBed.inject(MemoryStore);
    await store.loadAll();
  });

  it('loads status, facts and the first page of the history', () => {
    expect(store.status()?.counts.facts).toBe(1);
    expect(store.facts().map((f) => f.id)).toEqual(['a']);
    expect(store.episodes()).toHaveLength(EPISODE_PAGE);
    expect(store.episodesComplete()).toBe(false);
    expect(store.busy()).toBe(false);
  });

  it('pages through the history', async () => {
    await store.loadEpisodes(true);
    expect(api.episodeRequests.at(-1)).toBe(1000 - EPISODE_PAGE);
    expect(store.episodes()).toHaveLength(EPISODE_PAGE + 3);
    expect(store.episodesComplete()).toBe(true);
  });

  it('saves, pins and deletes facts', async () => {
    const saved = await store.saveFact({
      id: null,
      content: 'likes tea',
      projectId: null,
      pinned: false,
      sourceConversationId: null,
    });
    expect(saved?.content).toBe('likes tea');
    expect(store.facts()).toHaveLength(2);

    await store.togglePinned(store.facts()[1]);
    expect(api.saved.at(-1)).toMatchObject({ id: 'a', pinned: true });

    await store.deleteFact('a');
    expect(store.facts().map((f) => f.id)).not.toContain('a');

    await store.deleteFact('missing');
    expect(store.error()?.code).toBe('notFound');
  });

  it('searches only with text and clears memory with the confirmation phrase', async () => {
    await store.search('   ', ['fact']);
    expect(api.searches).toHaveLength(0);
    expect(store.results()).toBeNull();
    await store.search(' atlas ', ['fact', 'message']);
    expect(api.searches[0]).toEqual({ text: 'atlas', kinds: ['fact', 'message'], limit: 50 });
    expect(store.results()).toEqual([]);

    await store.clearAll();
    expect(api.clearedWith).toEqual([CLEAR_CONFIRMATION]);
  });

  it('ignores a cancelled export dialog', async () => {
    await store.exportMemory();
    expect(store.lastTransfer()).toBeNull();
  });

  it('splits highlighted snippets', () => {
    expect(snippetParts('a \u0002match\u0003 b')).toEqual([
      { text: 'a ', match: false },
      { text: 'match', match: true },
      { text: ' b', match: false },
    ]);
    expect(snippetParts('plain')).toEqual([{ text: 'plain', match: false }]);
  });
});

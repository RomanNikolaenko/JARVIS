import { TestBed } from '@angular/core/testing';
import type {
  Episode,
  EpisodeCursor,
  Fact,
  FactInput,
  MemoryStatus,
  Project,
  ProjectInput,
  SearchQuery,
  TransferReport,
} from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';
import {
  CLEAR_CONFIRMATION,
  EPISODE_PAGE,
  MEMORY_HISTORY_TIMEOUT_MS,
  MEMORY_SEARCH_TIMEOUT_MS,
  MemoryStore,
  snippetParts,
} from './memory.store';

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

const REPORT: TransferReport = {
  path: '/memory.json',
  counts: { conversations: 1, messages: 2, facts: 1, projects: 0, episodes: 3 },
  skipped: 0,
};

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

function project(id: string): Project {
  return { id, name: id, description: '', instructions: '', factCount: 0, conversationCount: 0, createdAtMs: 1, updatedAtMs: 1 };
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
  projects: Project[] = [];
  saved: FactInput[] = [];
  searches: SearchQuery[] = [];
  clearedWith: string[] = [];
  episodeRequests: (EpisodeCursor | null)[] = [];

  getMemoryStatus = vi.fn(async () => STATUS);
  listFacts = vi.fn(async () => this.facts);
  listProjects = vi.fn(async () => this.projects);
  saveProject = vi.fn(async (input: ProjectInput) => {
    const saved = {
      ...project(input.id ?? 'new'),
      name: input.name,
      description: input.description,
      instructions: input.instructions,
    };
    this.projects = [saved, ...this.projects.filter((entry) => entry.id !== saved.id)];
    return saved;
  });
  deleteProject = vi.fn(async (id: string) => {
    this.projects = this.projects.filter((entry) => entry.id !== id);
  });
  async listEpisodes(limit: number, before: EpisodeCursor | null): Promise<Episode[]> {
    this.episodeRequests.push(before);
    const start = before?.occurredAtMs ?? 1000;
    const count = before === null ? limit : 3;
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
  exportMemory = vi.fn(async (): Promise<TransferReport | null> => null);
  importMemory = vi.fn(async (): Promise<TransferReport | null> => null);
  clearEpisodes = vi.fn(async () => 100);
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

  afterEach(() => {
    TestBed.resetTestingModule();
    vi.useRealTimers();
  });

  it.each(['facts', 'projects'] as const)('keeps newer %s when an older read finishes last', async (kind) => {
    const oldFacts = deferred<Fact[]>();
    const oldProjects = deferred<Project[]>();
    if (kind === 'facts') api.listFacts.mockReturnValueOnce(oldFacts.promise);
    else api.listProjects.mockReturnValueOnce(oldProjects.promise);
    const first = kind === 'facts' ? store.loadFacts() : store.loadProjects();
    api.facts = [fact('new')];
    api.projects = [project('new')];
    await (kind === 'facts' ? store.loadFacts() : store.loadProjects());
    oldFacts.resolve([fact('old')]);
    oldProjects.resolve([project('old')]);
    await first;
    expect((kind === 'facts' ? store.facts() : store.projects()).map((entry) => entry.id)).toEqual(['new']);
  });

  it.each(['success', 'failure'])('ignores an old fact read %s after deletion', async (outcome) => {
    const pending = deferred<Fact[]>();
    api.listFacts.mockReturnValueOnce(pending.promise);
    const reading = store.loadFacts();
    await store.deleteFact('a');
    if (outcome === 'success') pending.resolve([fact('a')]);
    else pending.reject(new Error('Obsolete read failed'));
    await reading;
    expect(store.facts()).toEqual([]);
    expect(store.error()).toBeNull();
  });

  it('retains a confirmed fact update when refresh fails and an older list arrives', async () => {
    const pending = deferred<Fact[]>();
    api.listFacts.mockReturnValueOnce(pending.promise).mockRejectedValueOnce(new Error('Refresh unavailable'));
    const reading = store.loadFacts();
    const saved = await store.saveFact({
      id: 'a',
      content: 'updated',
      pinned: true,
      projectId: null,
      sourceConversationId: null,
    });
    pending.resolve([fact('a')]);
    await reading;
    expect(store.facts()).toEqual([saved]);
    expect(store.facts()[0].content).toBe('updated');
    expect(store.error()?.message).toBe('Refresh unavailable');
  });

  it('retains a confirmed project update even if refreshing the list fails', async () => {
    const pending = deferred<Project[]>();
    api.listProjects.mockReturnValueOnce(pending.promise).mockRejectedValueOnce(new Error('Projects unavailable'));
    const reading = store.loadProjects();
    const saved = await store.saveProject({ id: 'p', name: 'Updated', description: '', instructions: '' });
    pending.resolve([project('p')]);
    await reading;
    expect(store.projects()).toEqual([saved]);
    expect(store.projectNames().get('p')).toBe('Updated');
    expect(store.error()?.message).toBe('Projects unavailable');
  });

  it('does not restore a deleted project from an old list when refresh fails', async () => {
    store.projects.set([project('p')]);
    const pending = deferred<Project[]>();
    api.listProjects.mockReturnValueOnce(pending.promise).mockRejectedValueOnce(new Error('Projects unavailable'));
    const reading = store.loadProjects();
    await store.deleteProject('p');
    pending.resolve([project('p')]);
    await reading;
    expect(store.projects()).toEqual([]);
    expect(store.error()?.message).toBe('Projects unavailable');
  });

  it('does not publish lists after the store is destroyed', async () => {
    const pending = deferred<Fact[]>();
    api.listFacts.mockReturnValueOnce(pending.promise);
    const reading = store.loadFacts();
    TestBed.resetTestingModule();
    pending.resolve([fact('late')]);
    await reading;
    expect(store.facts().map((entry) => entry.id)).toEqual(['a']);
  });

  it.each(['success', 'failure'])('ignores stale search %s while a newer query is pending', async (outcome) => {
    const old = deferred<[]>();
    const current = deferred<[]>();
    const search = vi.spyOn(api, 'searchMemory').mockReturnValueOnce(old.promise).mockReturnValueOnce(current.promise);
    const first = store.search('old', ['fact']);
    const second = store.search('new', ['fact']);
    if (outcome === 'success') old.resolve([]);
    else old.reject(new Error('Old failure'));
    await first;
    expect(store.results()).toBeNull();
    expect(store.searchError()).toBeNull();
    expect(store.searching()).toBe(true);
    current.resolve([]);
    await second;
    expect(store.results()).toEqual([]);
    expect(store.searching()).toBe(false);
    expect(search).toHaveBeenCalledTimes(2);
  });

  it('shares identical searches but treats changed filters as a new request', async () => {
    const pending = deferred<[]>();
    const search = vi.spyOn(api, 'searchMemory').mockReturnValueOnce(pending.promise);
    const first = store.search(' atlas ', ['message', 'fact']);
    expect(store.search('atlas', ['fact', 'message', 'fact'])).toBe(first);
    await store.search('atlas', ['episode']);
    expect(search).toHaveBeenCalledTimes(2);
    pending.reject(new Error('Old filters failed'));
    await first;
    expect(store.results()).toEqual([]);
    expect(store.searchError()).toBeNull();
  });

  it.each(['blank', 'no-filters', 'draft-change'])('invalidates a pending search on %s', async (reason) => {
    const pending = deferred<[]>();
    const search = vi.spyOn(api, 'searchMemory').mockReturnValueOnce(pending.promise);
    const first = store.search('atlas', ['fact']);
    if (reason === 'draft-change') store.clearSearch();
    else await store.search(reason === 'blank' ? ' ' : 'atlas', reason === 'no-filters' ? [] : ['fact']);
    pending.resolve([]);
    await first;
    expect(search).toHaveBeenCalledTimes(1);
    expect(store.results()).toBeNull();
    expect(store.searching()).toBe(false);
  });

  it('allows retry after search timeout and ignores the late result', async () => {
    vi.useFakeTimers();
    const pending = deferred<[]>();
    vi.spyOn(api, 'searchMemory').mockReturnValueOnce(pending.promise);
    const first = store.search('atlas', ['fact']);
    await vi.advanceTimersByTimeAsync(MEMORY_SEARCH_TIMEOUT_MS);
    await first;
    expect(store.searchError()?.code).toBe('timeout');
    expect(store.searching()).toBe(false);
    await store.search('retry', ['fact']);
    pending.reject(new Error('Late error'));
    await vi.advanceTimersByTimeAsync(0);
    expect(store.searchError()).toBeNull();
    expect(store.results()).toEqual([]);
    expect(vi.getTimerCount()).toBe(0);
  });

  it('does not restore search results after clearing memory', async () => {
    const pending = deferred<[]>();
    vi.spyOn(api, 'searchMemory').mockReturnValueOnce(pending.promise);
    const first = store.search('atlas', ['fact']);
    await store.clearAll();
    pending.resolve([]);
    await first;
    expect(store.results()).toBeNull();
  });

  it('settles search on destruction without publishing late results', async () => {
    const pending = deferred<[]>();
    vi.spyOn(api, 'searchMemory').mockReturnValueOnce(pending.promise);
    const first = store.search('atlas', ['fact']);
    TestBed.resetTestingModule();
    await first;
    pending.resolve([]);
    await Promise.resolve();
    expect(store.results()).toBeNull();
    expect(store.searchError()).toBeNull();
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
    expect(api.episodeRequests.at(-1)).toEqual({ occurredAtMs: 1000 - EPISODE_PAGE, id: 'e900' });
    expect(store.episodes()).toHaveLength(EPISODE_PAGE + 3);
    expect(store.episodesComplete()).toBe(true);
  });

  it('shares repeated page requests and stops requesting after the final page', async () => {
    const pending = deferred<Episode[]>();
    const list = vi.spyOn(api, 'listEpisodes').mockReturnValueOnce(pending.promise);
    const first = store.loadEpisodes(true);
    expect(store.loadEpisodes(true)).toBe(first);
    expect(store.loadingEpisodes()).toBe(true);
    expect(list).toHaveBeenCalledTimes(1);
    pending.resolve([episode(899), episode(898)]);
    await first;
    expect(store.episodes()).toHaveLength(EPISODE_PAGE + 2);
    expect(store.loadingEpisodes()).toBe(false);
    await store.loadEpisodes(true);
    expect(list).toHaveBeenCalledTimes(1);
  });

  it.each(['success', 'failure'])('ignores an old next-page %s after a refresh', async (outcome) => {
    const old = deferred<Episode[]>();
    const current = deferred<Episode[]>();
    vi.spyOn(api, 'listEpisodes').mockReturnValueOnce(old.promise).mockReturnValueOnce(current.promise);
    const more = store.loadEpisodes(true);
    const refresh = store.loadEpisodes();
    expect(store.loadEpisodes()).toBe(refresh);
    expect(store.loadEpisodes(true)).toBe(refresh);
    if (outcome === 'success') old.resolve([episode(899)]);
    else old.reject(new Error('Old page failed'));
    await more;
    expect(store.loadingEpisodes()).toBe(true);
    expect(store.episodes()).toHaveLength(EPISODE_PAGE);
    expect(store.error()).toBeNull();
    current.resolve([episode(2000)]);
    await refresh;
    expect(store.episodes().map((entry) => entry.id)).toEqual(['e2000']);
    expect(store.loadingEpisodes()).toBe(false);
  });

  it('does not append duplicate entries from an overlapping page', async () => {
    vi.spyOn(api, 'listEpisodes').mockResolvedValueOnce([episode(900), episode(899), episode(899)]);
    await store.loadEpisodes(true);
    expect(store.episodes()).toHaveLength(EPISODE_PAGE + 1);
    expect(store.episodes().at(-1)?.id).toBe('e899');
  });

  it('does not restore deleted history from a pending page and coalesces deletion', async () => {
    const page = deferred<Episode[]>();
    const deletion = deferred<number>();
    const list = vi.spyOn(api, 'listEpisodes').mockReturnValueOnce(page.promise);
    api.clearEpisodes.mockReturnValueOnce(deletion.promise);
    const reading = store.loadEpisodes(true);
    const clearing = store.clearEpisodes();
    expect(store.clearEpisodes()).toBe(clearing);
    await store.loadEpisodes();
    expect(list).toHaveBeenCalledTimes(1);
    expect(store.clearingEpisodes()).toBe(true);
    deletion.resolve(100);
    await clearing;
    page.resolve([episode(899)]);
    await reading;
    expect(api.clearEpisodes).toHaveBeenCalledTimes(1);
    expect(store.episodes()).toEqual([]);
    expect(store.episodesComplete()).toBe(true);
    expect(store.clearingEpisodes()).toBe(false);
  });

  it('retains history when deletion fails and permits retry', async () => {
    api.clearEpisodes.mockRejectedValueOnce(new Error('Cannot delete history'));
    await store.clearEpisodes();
    expect(store.episodes()).toHaveLength(EPISODE_PAGE);
    expect(store.error()?.message).toBe('Cannot delete history');
    expect(store.clearingEpisodes()).toBe(false);
    await store.clearEpisodes();
    expect(store.episodes()).toEqual([]);
  });

  it('retries after a history timeout without accepting the late page', async () => {
    vi.useFakeTimers();
    const old = deferred<Episode[]>();
    vi.spyOn(api, 'listEpisodes')
      .mockReturnValueOnce(old.promise)
      .mockResolvedValueOnce([episode(2000)]);
    const reading = store.loadEpisodes();
    await vi.advanceTimersByTimeAsync(MEMORY_HISTORY_TIMEOUT_MS);
    await reading;
    expect(store.error()?.code).toBe('timeout');
    expect(store.loadingEpisodes()).toBe(false);
    await store.loadEpisodes();
    old.resolve([episode(1)]);
    await vi.advanceTimersByTimeAsync(0);
    expect(store.episodes().map((entry) => entry.id)).toEqual(['e2000']);
    expect(store.error()).toBeNull();
    expect(vi.getTimerCount()).toBe(0);
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

  it.each(['export', 'import'] as const)('shares duplicate %s requests and rejects conflicting operations', async (kind) => {
    const pending = deferred<TransferReport | null>();
    const apiCall = kind === 'export' ? api.exportMemory : api.importMemory;
    apiCall.mockReturnValueOnce(pending.promise);
    const invoke = () => (kind === 'export' ? store.exportMemory() : store.importMemory());
    const first = invoke();
    expect(invoke()).toBe(first);
    expect(store.dataOperation()).toBe(kind);
    await (kind === 'export' ? store.importMemory() : store.exportMemory());
    await store.clearAll();
    expect(apiCall).toHaveBeenCalledTimes(1);
    expect(kind === 'export' ? api.importMemory : api.exportMemory).not.toHaveBeenCalled();
    expect(api.clearedWith).toEqual([]);
    pending.resolve(REPORT);
    await first;
    expect(store.lastTransfer()).toEqual({ kind, report: REPORT });
    expect(store.busy()).toBe(false);
    expect(store.dataOperation()).toBeNull();
  });

  it('retains the import guard through refresh and keeps refresh failures visible', async () => {
    const status = deferred<MemoryStatus>();
    api.importMemory.mockResolvedValueOnce(REPORT);
    api.getMemoryStatus.mockReturnValueOnce(status.promise);
    api.listFacts.mockRejectedValueOnce(new Error('Facts unavailable'));
    const importing = store.importMemory();
    await vi.waitFor(() => expect(store.error()?.message).toBe('Facts unavailable'));
    expect(store.importMemory()).toBe(importing);
    await store.exportMemory();
    expect(api.exportMemory).not.toHaveBeenCalled();
    status.resolve(STATUS);
    await importing;
    expect(store.error()?.message).toBe('Facts unavailable');
    expect(store.lastTransfer()?.report).toEqual(REPORT);
    expect(store.dataError()).toBeNull();
  });

  it('does not erase a transfer failure when an unrelated read succeeds', async () => {
    api.exportMemory.mockRejectedValueOnce(new Error('Cannot write export'));
    await store.exportMemory();
    await store.refreshStatus();
    expect(store.dataError()?.message).toBe('Cannot write export');
    expect(store.busy()).toBe(false);
    api.exportMemory.mockResolvedValueOnce(REPORT);
    await store.exportMemory();
    expect(store.dataError()).toBeNull();
    expect(store.lastTransfer()?.report).toEqual(REPORT);
  });

  it.each(['export', 'import'] as const)('clears an old report when a new %s is cancelled', async (kind) => {
    store.lastTransfer.set({ kind: 'export', report: REPORT });
    await (kind === 'export' ? store.exportMemory() : store.importMemory());
    expect(store.lastTransfer()).toBeNull();
    expect(store.dataError()).toBeNull();
    expect(store.dataOperation()).toBeNull();
  });

  it('blocks transfers and repeat deletion while clear is pending', async () => {
    const pending = deferred<void>();
    const clear = vi.spyOn(api, 'clearMemory').mockReturnValueOnce(pending.promise);
    const deleting = store.clearAll();
    expect(store.clearAll()).toBe(deleting);
    await store.exportMemory();
    await store.importMemory();
    expect(clear).toHaveBeenCalledTimes(1);
    expect(api.exportMemory).not.toHaveBeenCalled();
    expect(api.importMemory).not.toHaveBeenCalled();
    pending.resolve();
    await deleting;
    expect(store.busy()).toBe(false);
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

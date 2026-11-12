import { TestBed } from '@angular/core/testing';
import type {
  AcceptFactSuggestion,
  Episode,
  EpisodeCursor,
  Fact,
  FactInput,
  FactSuggestion,
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
  MEMORY_READ_TIMEOUT_MS,
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

function suggestion(id: string): FactSuggestion {
  return {
    id,
    content: `suggested ${id}`,
    projectId: null,
    sourceConversationId: 'c1',
    sourceConversationTitle: 'Chat',
    createdAtMs: 1,
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
  encryption: 'off',
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
  suggestions: FactSuggestion[] = [suggestion('s1')];
  suggestionHandler: ((count: number) => void) | null = null;
  listFactSuggestions = vi.fn(async () => this.suggestions);
  acceptFactSuggestion = vi.fn(async (input: AcceptFactSuggestion): Promise<Fact> => {
    this.suggestions = this.suggestions.filter((s) => s.id !== input.id);
    return { ...fact(`from-${input.id}`, input.pinned), content: input.content, source: 'conversation' };
  });
  rejectFactSuggestion = vi.fn(async (id: string) => {
    this.suggestions = this.suggestions.filter((s) => s.id !== id);
  });
  onFactSuggestions = vi.fn(async (handler: (count: number) => void) => {
    this.suggestionHandler = handler;
    return () => (this.suggestionHandler = null);
  });
  prepareMemoryEncryption = vi.fn(async () => ({ code: '111111-222222-333333-444444-555555-666666-777777-888888' }));
  encryptMemory = vi.fn(async (_confirmation: string) => {
    this.getMemoryStatus.mockResolvedValue({ ...STATUS, encryption: 'on' });
  });
  unlockMemory = vi.fn(async (_code: string) => {
    this.getMemoryStatus.mockResolvedValue({ ...STATUS, encryption: 'on' });
  });
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

  it('loads fact suggestions and follows the count event', async () => {
    await store.start();
    expect(store.suggestions().map((s) => s.id)).toEqual(['s1']);
    expect(store.pendingSuggestions()).toBe(1);
    api.suggestions = [suggestion('s2'), suggestion('s1')];
    api.suggestionHandler?.(2);
    expect(store.pendingSuggestions()).toBe(2);
    await vi.waitFor(() => expect(store.suggestions().map((s) => s.id)).toEqual(['s2', 's1']));
  });

  it('accepts an edited suggestion as a fact', async () => {
    await store.start();
    await store.acceptSuggestion({ id: 's1', content: 'edited', pinned: true, projectId: null });
    expect(api.acceptFactSuggestion).toHaveBeenCalledWith({ id: 's1', content: 'edited', pinned: true, projectId: null });
    expect(store.suggestions()).toEqual([]);
    expect(store.pendingSuggestions()).toBe(0);
    expect(store.facts()[0]).toMatchObject({ id: 'from-s1', content: 'edited', pinned: true });
  });

  it('rejects a suggestion', async () => {
    await store.start();
    await store.rejectSuggestion('s1');
    expect(api.rejectFactSuggestion).toHaveBeenCalledWith('s1');
    expect(store.suggestions()).toEqual([]);
    expect(store.pendingSuggestions()).toBe(0);
  });

  it('keeps a suggestion when accepting it fails', async () => {
    await store.start();
    api.acceptFactSuggestion.mockRejectedValueOnce(new IpcError('notFound', 'the fact suggestion does not exist'));
    await store.acceptSuggestion({ id: 's1', content: 'x', pinned: false, projectId: null });
    expect(store.suggestions().map((s) => s.id)).toEqual(['s1']);
    expect(store.error()?.message).toBe('the fact suggestion does not exist');
  });

  // Review finding #6: the reload after each count event went through the
  // screen's busy/error handling.
  it('reloads suggestions in the background without hiding an error or blocking controls', async () => {
    await store.start();
    api.acceptFactSuggestion.mockRejectedValueOnce(new IpcError('notFound', 'the fact suggestion does not exist'));
    await store.acceptSuggestion({ id: 's1', content: 'x', pinned: false, projectId: null });
    const reading = deferred<FactSuggestion[]>();
    api.listFactSuggestions.mockReturnValueOnce(reading.promise);
    api.suggestionHandler?.(0);
    expect(store.busy()).toBe(false);
    reading.resolve([]);
    await vi.waitFor(() => expect(store.suggestions()).toEqual([]));
    expect(store.error()?.message).toBe('the fact suggestion does not exist');
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
    await vi.waitFor(() => expect(store.refreshError()?.message).toBe('Refresh unavailable'));
    expect(store.error()).toBeNull();
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
    await vi.waitFor(() => expect(store.refreshError()?.message).toBe('Projects unavailable'));
    expect(store.error()).toBeNull();
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
    await vi.waitFor(() => expect(store.refreshError()?.message).toBe('Projects unavailable'));
    expect(store.error()).toBeNull();
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

  it.each(['status', 'facts', 'projects', 'suggestions'] as const)(
    'bounds a hung %s read and ignores its late result after an explicit reload',
    async (kind) => {
      const oldStatus = deferred<MemoryStatus>();
      const oldFacts = deferred<Fact[]>();
      const oldProjects = deferred<Project[]>();
      const oldSuggestions = deferred<FactSuggestion[]>();
      const freshStatus = { ...STATUS, sizeBytes: 8192 };
      api.getMemoryStatus.mockResolvedValue(freshStatus);
      api.facts = [fact('fresh')];
      api.projects = [project('fresh')];
      api.suggestions = [suggestion('fresh')];
      let read: Promise<void>;
      vi.useFakeTimers();
      try {
        if (kind === 'status') {
          api.getMemoryStatus.mockReturnValueOnce(oldStatus.promise);
          read = store.refreshStatus();
        } else if (kind === 'facts') {
          api.listFacts.mockReturnValueOnce(oldFacts.promise);
          read = store.loadFacts();
        } else if (kind === 'projects') {
          api.listProjects.mockReturnValueOnce(oldProjects.promise);
          read = store.loadProjects();
        } else {
          api.listFactSuggestions.mockReturnValueOnce(oldSuggestions.promise);
          read = store.loadSuggestions();
        }
        await vi.advanceTimersByTimeAsync(MEMORY_READ_TIMEOUT_MS);
        await read;
        expect(store.error()?.code).toBe('timeout');
        expect(store.busy()).toBe(false);
        await store.loadAll();
        oldStatus.resolve(STATUS);
        oldFacts.resolve([fact('old')]);
        oldProjects.resolve([project('old')]);
        oldSuggestions.resolve([suggestion('old')]);
        await vi.advanceTimersByTimeAsync(0);
        expect(store.status()).toBe(freshStatus);
        expect(store.facts().map((entry) => entry.id)).toEqual(['fresh']);
        expect(store.projects().map((entry) => entry.id)).toEqual(['fresh']);
        expect(store.suggestions().map((entry) => entry.id)).toEqual(['fresh']);
        expect(store.error()).toBeNull();
        expect(store.busy()).toBe(false);
        expect(vi.getTimerCount()).toBe(0);
      } finally {
        vi.useRealTimers();
      }
    }
  );

  it('does not let an older status read replace a newer status or error', async () => {
    const old = deferred<MemoryStatus>();
    const fresh = { ...STATUS, sizeBytes: 8192 };
    api.getMemoryStatus.mockReturnValueOnce(old.promise).mockResolvedValueOnce(fresh);
    const first = store.refreshStatus();
    await store.refreshStatus();
    old.resolve(STATUS);
    await first;
    expect(store.status()).toBe(fresh);
    expect(store.error()).toBeNull();
  });

  it.each(['status', 'facts', 'projects', 'suggestions'] as const)(
    'settles a pending %s read on destruction without applying a late response',
    async (kind) => {
      const oldStatus = deferred<MemoryStatus>();
      const oldFacts = deferred<Fact[]>();
      const oldProjects = deferred<Project[]>();
      const oldSuggestions = deferred<FactSuggestion[]>();
      let read: Promise<void>;
      if (kind === 'status') {
        api.getMemoryStatus.mockReturnValueOnce(oldStatus.promise);
        read = store.refreshStatus();
      } else if (kind === 'facts') {
        api.listFacts.mockReturnValueOnce(oldFacts.promise);
        read = store.loadFacts();
      } else if (kind === 'projects') {
        api.listProjects.mockReturnValueOnce(oldProjects.promise);
        read = store.loadProjects();
      } else {
        api.listFactSuggestions.mockReturnValueOnce(oldSuggestions.promise);
        read = store.loadSuggestions();
      }
      const previous = {
        status: store.status(),
        facts: store.facts(),
        projects: store.projects(),
        suggestions: store.suggestions(),
      };
      TestBed.resetTestingModule();
      await read;
      oldStatus.resolve({ ...STATUS, sizeBytes: 9999 });
      oldFacts.resolve([fact('late')]);
      oldProjects.resolve([project('late')]);
      oldSuggestions.resolve([suggestion('late')]);
      await Promise.resolve();
      expect(store.status()).toBe(previous.status);
      expect(store.facts()).toBe(previous.facts);
      expect(store.projects()).toBe(previous.projects);
      expect(store.suggestions()).toBe(previous.suggestions);
      expect(store.error()).toBeNull();
      expect(store.busy()).toBe(false);
    }
  );

  it('returns a confirmed fact before a stalled background refresh without blocking controls or retrying the write', async () => {
    const facts = deferred<Fact[]>();
    const projects = deferred<Project[]>();
    api.listFacts.mockReturnValueOnce(facts.promise);
    api.listProjects.mockReturnValueOnce(projects.promise);
    const save = vi.spyOn(api, 'saveFact');
    vi.useFakeTimers();
    try {
      const saved = await store.saveFact({
        id: null,
        content: 'Saved already',
        projectId: null,
        pinned: false,
        sourceConversationId: null,
      });
      expect(saved?.content).toBe('Saved already');
      expect(store.facts()).toContainEqual(saved);
      expect(store.busy()).toBe(false);
      expect(store.error()).toBeNull();
      await vi.advanceTimersByTimeAsync(MEMORY_READ_TIMEOUT_MS);
      expect(store.refreshError()?.code).toBe('timeout');
      expect(store.error()).toBeNull();
      expect(store.busy()).toBe(false);
      expect(save).toHaveBeenCalledOnce();
      await store.loadAll();
      facts.resolve([fact('obsolete')]);
      projects.reject(new Error('Late background failure'));
      await vi.advanceTimersByTimeAsync(0);
      expect(store.facts()).toContainEqual(saved);
      expect(store.refreshError()).toBeNull();
      expect(store.error()).toBeNull();
      expect(save).toHaveBeenCalledOnce();
    } finally {
      vi.useRealTimers();
    }
  });

  it('returns a confirmed project before a stalled background refresh', async () => {
    const projects = deferred<Project[]>();
    api.listProjects.mockReturnValueOnce(projects.promise);
    const saved = await store.saveProject({ id: null, name: 'Saved project', description: '', instructions: '' });
    expect(saved?.name).toBe('Saved project');
    expect(store.projects()).toEqual([saved]);
    expect(store.busy()).toBe(false);
    expect(api.saveProject).toHaveBeenCalledOnce();
    projects.reject(new Error('Read unavailable'));
    await vi.waitFor(() => expect(store.refreshError()?.message).toBe('Read unavailable'));
    expect(store.error()).toBeNull();
    expect(store.projects()).toEqual([saved]);
  });

  it('keeps a background refresh failure when its sibling read succeeds', async () => {
    const facts = deferred<Fact[]>();
    api.listFacts.mockReturnValueOnce(facts.promise);
    api.listProjects.mockRejectedValueOnce(new Error('Projects refresh failed'));
    const saved = await store.saveFact({
      id: 'a',
      content: 'Updated',
      projectId: null,
      pinned: false,
      sourceConversationId: null,
    });
    await vi.waitFor(() => expect(store.refreshError()?.message).toBe('Projects refresh failed'));
    facts.resolve([saved!]);
    await Promise.resolve();
    expect(store.refreshError()?.message).toBe('Projects refresh failed');
    expect(store.error()).toBeNull();
  });

  it('does not let an obsolete background refresh mask a newer mutation error', async () => {
    const old = deferred<Fact[]>();
    api.listFacts.mockReturnValueOnce(old.promise);
    await store.saveFact({ id: 'a', content: 'Confirmed', projectId: null, pinned: false, sourceConversationId: null });
    vi.spyOn(api, 'saveFact').mockRejectedValueOnce({ code: 'io', message: 'New write failed' });
    expect(
      await store.saveFact({ id: null, content: 'Not confirmed', projectId: null, pinned: false, sourceConversationId: null })
    ).toBeNull();
    old.reject(new Error('Old refresh failed'));
    await vi.waitFor(() => expect(store.refreshError()?.message).toBe('Old refresh failed'));
    expect(store.error()?.message).toBe('New write failed');
  });

  it('ignores a background refresh error from an older batch', async () => {
    const old = deferred<Fact[]>();
    api.listFacts.mockReturnValueOnce(old.promise);
    await store.saveFact({ id: 'a', content: 'Confirmed', projectId: null, pinned: false, sourceConversationId: null });
    await store.saveProject({ id: 'p', name: 'Newer project', description: '', instructions: '' });
    old.reject(new Error('Obsolete batch failed'));
    await Promise.resolve();
    await Promise.resolve();
    expect(store.refreshError()).toBeNull();
    expect(store.error()).toBeNull();
    expect(store.projects()[0].name).toBe('Newer project');
  });

  it.each(['fact', 'project'] as const)('does not return a saved %s or start a refresh after store destruction', async (kind) => {
    const factWrite = deferred<Fact>();
    const projectWrite = deferred<Project>();
    vi.spyOn(api, 'saveFact').mockReturnValueOnce(factWrite.promise);
    api.saveProject.mockReturnValueOnce(projectWrite.promise);
    api.listFacts.mockClear();
    api.listProjects.mockClear();
    const saving =
      kind === 'fact'
        ? store.saveFact({ id: null, content: 'Saved late', projectId: null, pinned: false, sourceConversationId: null })
        : store.saveProject({ id: null, name: 'Saved late', description: '', instructions: '' });
    const previousFacts = store.facts();
    const previousProjects = store.projects();
    TestBed.resetTestingModule();
    factWrite.resolve(fact('late'));
    projectWrite.resolve(project('late'));
    expect(await saving).toBeNull();
    expect(store.facts()).toBe(previousFacts);
    expect(store.projects()).toBe(previousProjects);
    expect(api.listFacts).not.toHaveBeenCalled();
    expect(api.listProjects).not.toHaveBeenCalled();
    expect(store.busy()).toBe(false);
  });

  it('reports fact deletion only after a confirmed success and preserves a failed deletion error', async () => {
    expect(await store.deleteFact('missing')).toBe(false);
    expect(store.facts().map((entry) => entry.id)).toEqual(['a']);
    expect(store.error()).toMatchObject({ code: 'notFound', message: 'the fact does not exist' });
    expect(await store.deleteFact('a')).toBe(true);
    expect(store.facts()).toEqual([]);
    expect(store.error()).toBeNull();
  });

  it('does not dispatch memory writes while a data transfer is active', async () => {
    const transfer = deferred<TransferReport | null>();
    api.exportMemory.mockReturnValueOnce(transfer.promise);
    const exporting = store.exportMemory();
    const saveFact = vi.spyOn(api, 'saveFact');
    const deleteFact = vi.spyOn(api, 'deleteFact');
    expect(
      await store.saveFact({ id: null, content: 'Do not overlap', projectId: null, pinned: false, sourceConversationId: null })
    ).toBeNull();
    expect(await store.saveProject({ id: null, name: 'Do not overlap', description: '', instructions: '' })).toBeNull();
    expect(await store.deleteFact('a')).toBe(false);
    await store.deleteProject('p');
    expect(saveFact).not.toHaveBeenCalled();
    expect(api.saveProject).not.toHaveBeenCalled();
    expect(deleteFact).not.toHaveBeenCalled();
    expect(api.deleteProject).not.toHaveBeenCalled();
    transfer.resolve(null);
    await exporting;
  });

  it('clears acknowledged memory immediately and fences reads from before or during deletion when reloading fails', async () => {
    const oldFacts = deferred<Fact[]>();
    const oldProjects = deferred<Project[]>();
    const oldSuggestions = deferred<FactSuggestion[]>();
    const oldStatus = deferred<MemoryStatus>();
    const cleared = deferred<void>();
    store.projects.set([project('before-clear')]);
    api.listFacts.mockReturnValueOnce(oldFacts.promise).mockRejectedValueOnce(new Error('Post-clear facts unavailable'));
    api.listProjects.mockReturnValueOnce(oldProjects.promise).mockRejectedValueOnce(new Error('Post-clear projects unavailable'));
    api.listFactSuggestions
      .mockReturnValueOnce(oldSuggestions.promise)
      .mockRejectedValueOnce(new Error('Post-clear suggestions unavailable'));
    api.getMemoryStatus.mockReturnValueOnce(oldStatus.promise).mockRejectedValueOnce(new Error('Post-clear status unavailable'));
    vi.spyOn(api, 'listEpisodes').mockResolvedValueOnce([]);
    const clear = vi.spyOn(api, 'clearMemory').mockReturnValueOnce(cleared.promise);
    const saved = await store.saveFact({
      id: 'a',
      content: 'Confirmed before clear',
      pinned: false,
      projectId: null,
      sourceConversationId: null,
    });
    const suggestions = store.loadSuggestions();
    const clearing = store.clearAll();
    expect(store.facts()).toContainEqual(saved);
    expect(store.projects().map((entry) => entry.id)).toEqual(['before-clear']);
    expect(store.suggestions()).toHaveLength(1);
    expect(store.status()).toBe(STATUS);
    // A screen can request status while the native clear is already pending.
    const status = store.refreshStatus();
    cleared.resolve();
    await clearing;
    expect(store.facts()).toEqual([]);
    expect(store.projects()).toEqual([]);
    expect(store.suggestions()).toEqual([]);
    expect(store.pendingSuggestions()).toBe(0);
    expect(store.status()).toBeNull();
    expect(store.dataError()).toBeNull();
    expect(store.error()?.message).toContain('Post-clear');
    oldFacts.resolve([fact('deleted')]);
    oldProjects.resolve([project('deleted')]);
    oldSuggestions.resolve([suggestion('deleted')]);
    oldStatus.resolve(STATUS);
    await Promise.all([suggestions, status]);
    await Promise.resolve();
    expect(store.facts()).toEqual([]);
    expect(store.projects()).toEqual([]);
    expect(store.suggestions()).toEqual([]);
    expect(store.pendingSuggestions()).toBe(0);
    expect(store.status()).toBeNull();
    expect(store.refreshError()).toBeNull();
    expect(clear).toHaveBeenCalledOnce();
    expect(store.busy()).toBe(false);
  });

  it('does not restore pre-import background lists after loading the imported memory', async () => {
    const oldFacts = deferred<Fact[]>();
    const oldProjects = deferred<Project[]>();
    const oldSuggestions = deferred<FactSuggestion[]>();
    const report = deferred<TransferReport | null>();
    api.listFacts.mockReturnValueOnce(oldFacts.promise);
    api.listProjects.mockReturnValueOnce(oldProjects.promise);
    api.listFactSuggestions.mockReturnValueOnce(oldSuggestions.promise);
    await store.saveFact({ id: 'a', content: 'Before import', pinned: false, projectId: null, sourceConversationId: null });
    const suggestions = store.loadSuggestions();
    api.importMemory.mockReturnValueOnce(report.promise);
    const importing = store.importMemory();
    expect(store.facts()[0].content).toBe('Before import');
    api.facts = [fact('imported')];
    api.projects = [project('imported')];
    api.suggestions = [suggestion('imported')];
    report.resolve(REPORT);
    await importing;
    oldFacts.resolve([fact('old')]);
    oldProjects.resolve([project('old')]);
    oldSuggestions.resolve([suggestion('old')]);
    await suggestions;
    await Promise.resolve();
    expect(store.facts().map((entry) => entry.id)).toEqual(['imported']);
    expect(store.projects().map((entry) => entry.id)).toEqual(['imported']);
    expect(store.suggestions().map((entry) => entry.id)).toEqual(['imported']);
    expect(store.pendingSuggestions()).toBe(1);
    expect(store.refreshError()).toBeNull();
    expect(store.dataError()).toBeNull();
    expect(api.importMemory).toHaveBeenCalledOnce();
  });

  it('retains cached memory when clearing is refused without reviving an older background list', async () => {
    const old = deferred<Fact[]>();
    api.listFacts.mockReturnValueOnce(old.promise);
    const saved = await store.saveFact({
      id: 'a',
      content: 'Still exists',
      pinned: false,
      projectId: null,
      sourceConversationId: null,
    });
    vi.spyOn(api, 'clearMemory').mockRejectedValueOnce(new Error('Clear refused'));
    await store.clearAll();
    expect(store.facts()).toEqual([saved]);
    expect(store.dataError()?.message).toBe('Clear refused');
    old.resolve([fact('obsolete')]);
    await Promise.resolve();
    expect(store.facts()).toEqual([saved]);
    expect(store.status()).toBe(STATUS);
  });

  it.each(['failure', 'timeout'] as const)(
    'removes project facts on delete acknowledgement despite a facts-refresh %s',
    async (outcome) => {
      const projects = [project('deleted'), project('kept')];
      const kept = { ...fact('kept'), projectId: 'kept' };
      const unscoped = fact('unscoped');
      api.projects = projects;
      store.projects.set(projects);
      store.facts.set([{ ...fact('deleted'), projectId: 'deleted' }, kept, unscoped]);
      const pending = deferred<Fact[]>();
      if (outcome === 'failure') api.listFacts.mockRejectedValueOnce(new Error('Facts refresh unavailable'));
      else api.listFacts.mockReturnValueOnce(pending.promise);
      vi.useFakeTimers();
      try {
        await store.deleteProject('deleted');
        expect(store.projects().map((entry) => entry.id)).toEqual(['kept']);
        expect(store.facts()).toEqual([kept, unscoped]);
        expect(store.busy()).toBe(false);
        if (outcome === 'timeout') await vi.advanceTimersByTimeAsync(MEMORY_READ_TIMEOUT_MS);
        else await vi.advanceTimersByTimeAsync(0);
        expect(store.refreshError()?.message).toContain(outcome === 'timeout' ? 'did not respond' : 'Facts refresh unavailable');
        expect(store.facts()).toEqual([kept, unscoped]);
        expect(api.deleteProject).toHaveBeenCalledExactlyOnceWith('deleted');
      } finally {
        vi.useRealTimers();
      }
    }
  );

  it.each(['export', 'import', 'clear'] as const)(
    'does not publish a late native %s acknowledgement after disposal',
    async (kind) => {
      const report = deferred<TransferReport | null>();
      const clear = deferred<void>();
      api.exportMemory.mockReturnValueOnce(report.promise);
      api.importMemory.mockReturnValueOnce(report.promise);
      vi.spyOn(api, 'clearMemory').mockReturnValueOnce(clear.promise);
      const facts = store.facts();
      const projects = store.projects();
      const suggestions = store.suggestions();
      const status = store.status();
      const pending = kind === 'export' ? store.exportMemory() : kind === 'import' ? store.importMemory() : store.clearAll();
      api.listFacts.mockClear();
      api.listProjects.mockClear();
      TestBed.resetTestingModule();
      report.resolve(REPORT);
      clear.resolve();
      await pending;
      expect(store.facts()).toBe(facts);
      expect(store.projects()).toBe(projects);
      expect(store.suggestions()).toBe(suggestions);
      expect(store.status()).toBe(status);
      expect(store.lastTransfer()).toBeNull();
      expect(store.dataError()).toBeNull();
      expect(api.listFacts).not.toHaveBeenCalled();
      expect(api.listProjects).not.toHaveBeenCalled();
    }
  );

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

  it('prepares the recovery code and encrypts with its last group', async () => {
    expect(await store.prepareEncryption()).toBe('111111-222222-333333-444444-555555-666666-777777-888888');
    expect(await store.encrypt('888888')).toBe(true);
    expect(api.encryptMemory).toHaveBeenCalledWith('888888');
    expect(store.status()?.encryption).toBe('on');
    expect(store.dataOperation()).toBeNull();
    expect(store.busy()).toBe(false);
  });

  it('keeps an encryption failure visible and reports it', async () => {
    api.encryptMemory.mockRejectedValueOnce(new IpcError('limitExceeded', 'Stop the running replies'));
    expect(await store.encrypt('888888')).toBe(false);
    expect(store.dataError()?.message).toBe('Stop the running replies');
    expect(store.status()?.encryption).toBe('off');
  });

  it('reports a keychain failure while preparing and refreshes the status', async () => {
    api.prepareMemoryEncryption.mockRejectedValueOnce(new IpcError('unavailable', 'Encryption needs the system keychain'));
    api.getMemoryStatus.mockResolvedValue({ ...STATUS, encryption: 'unsupported' });
    expect(await store.prepareEncryption()).toBeNull();
    expect(store.dataError()?.message).toBe('Encryption needs the system keychain');
    await vi.waitFor(() => expect(store.status()?.encryption).toBe('unsupported'));
  });

  it('unlocks with the recovery code and reloads memory', async () => {
    api.getMemoryStatus.mockResolvedValue({ ...STATUS, available: false, encryption: 'locked' });
    await store.refreshStatus();
    api.unlockMemory.mockRejectedValueOnce(new IpcError('validation', 'This recovery code does not open the memory database'));
    expect(await store.unlock('1')).toBe(false);
    expect(store.unlockError()?.message).toBe('This recovery code does not open the memory database');
    api.listFacts.mockClear();
    expect(await store.unlock('111111-222222-333333-444444-555555-666666-777777-888888')).toBe(true);
    expect(api.unlockMemory).toHaveBeenLastCalledWith('111111-222222-333333-444444-555555-666666-777777-888888');
    expect(store.unlockError()).toBeNull();
    expect(store.unlocking()).toBe(false);
    expect(store.status()?.encryption).toBe('on');
    expect(api.listFacts).toHaveBeenCalled();
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

import { DestroyRef, Injectable, computed, inject, signal } from '@angular/core';
import type {
  Episode,
  Fact,
  FactInput,
  MemoryKind,
  MemoryStatus,
  Project,
  ProjectInput,
  SearchHit,
  TransferReport,
} from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';
import { withRequestDeadline } from '../ipc/request-deadline';

/** Phrase the core requires to delete all memory (see `memory_commands.rs`). */
export const CLEAR_CONFIRMATION = 'DELETE ALL MEMORY';
export const EPISODE_PAGE = 100;
export const MAX_FACT_LENGTH = 2000;
export const MEMORY_SEARCH_TIMEOUT_MS = 30_000;
export const MEMORY_HISTORY_TIMEOUT_MS = 30_000;

/** Splits a search snippet into plain and highlighted parts (U+0002 … U+0003). */
export function snippetParts(snippet: string): { text: string; match: boolean }[] {
  const parts: { text: string; match: boolean }[] = [];
  let match = false;
  let current = '';
  for (const char of snippet) {
    if (char === '\u0002' || char === '\u0003') {
      if (current) {
        parts.push({ text: current, match });
      }
      current = '';
      match = char === '\u0002';
    } else {
      current += char;
    }
  }
  if (current) {
    parts.push({ text: current, match });
  }
  return parts;
}

/** State of the Memory screen: facts, projects, action history, search. */
@Injectable({ providedIn: 'root' })
export class MemoryStore {
  private readonly api = inject(JarvisApi);
  private readonly destroyRef = inject(DestroyRef);
  private pending = 0;
  private readonly listEpochs = { facts: 0, projects: 0 };
  private episodeEpoch = 0;
  private episodeRequest: { more: boolean; promise: Promise<void> } | null = null;
  private clearingHistory: Promise<void> | null = null;
  private searchEpoch = 0;
  private searchRequest: { key: string; promise: Promise<void> } | null = null;
  private dataRequest: { kind: 'export' | 'import' | 'clear'; promise: Promise<void> } | null = null;

  readonly status = signal<MemoryStatus | null>(null);
  readonly facts = signal<readonly Fact[]>([]);
  readonly projects = signal<readonly Project[]>([]);
  readonly episodes = signal<readonly Episode[]>([]);
  readonly episodesComplete = signal(false);
  readonly loadingEpisodes = signal(false);
  readonly clearingEpisodes = signal(false);
  readonly results = signal<readonly SearchHit[] | null>(null);
  readonly searching = signal(false);
  readonly searchError = signal<IpcError | null>(null);
  readonly error = signal<IpcError | null>(null);
  readonly busy = signal(false);
  readonly lastTransfer = signal<{ kind: 'export' | 'import'; report: TransferReport } | null>(null);
  readonly dataOperation = signal<'export' | 'import' | 'clear' | null>(null);
  readonly dataError = signal<IpcError | null>(null);

  readonly projectNames = computed(() => new Map(this.projects().map((p) => [p.id, p.name] as const)));

  async loadAll(): Promise<void> {
    await Promise.all([this.refreshStatus(), this.loadFacts(), this.loadProjects(), this.loadEpisodes()]);
  }

  refreshStatus(): Promise<void> {
    return this.run(async () => this.status.set(await this.api.getMemoryStatus()));
  }

  loadFacts(): Promise<void> {
    return this.readList(
      'facts',
      () => this.api.listFacts(),
      (facts) => this.facts.set(facts)
    );
  }

  loadProjects(): Promise<void> {
    return this.readList(
      'projects',
      () => this.api.listProjects(),
      (projects) => this.projects.set(projects)
    );
  }

  private readList<T>(key: keyof typeof this.listEpochs, read: () => Promise<T>, apply: (value: T) => void): Promise<void> {
    const epoch = ++this.listEpochs[key];
    const current = () => epoch === this.listEpochs[key] && !this.destroyRef.destroyed;
    return this.run(async () => {
      const value = await read();
      if (current()) apply(value);
    }, current);
  }

  /** First page, or the next one with `more`. */
  loadEpisodes(more = false): Promise<void> {
    if (this.destroyRef.destroyed || this.clearingEpisodes()) return Promise.resolve();
    if (this.episodeRequest && (more || !this.episodeRequest.more)) return this.episodeRequest.promise;
    if (more && this.episodesComplete()) return Promise.resolve();
    const epoch = ++this.episodeEpoch;
    const isCurrent = () => epoch === this.episodeEpoch && !this.destroyRef.destroyed;
    const current = more ? this.episodes() : [];
    const last = current.at(-1);
    const before = last ? { occurredAtMs: last.occurredAtMs, id: last.id } : null;
    this.loadingEpisodes.set(true);
    const promise = this.run(async () => {
      const page = await withRequestDeadline(
        () => this.api.listEpisodes(EPISODE_PAGE, before),
        MEMORY_HISTORY_TIMEOUT_MS,
        'Action history',
        this.destroyRef
      );
      if (!isCurrent()) return;
      const seen = new Set(current.map((entry) => entry.id));
      const added = page.filter((entry) => {
        if (seen.has(entry.id)) return false;
        seen.add(entry.id);
        return true;
      });
      this.episodes.set([...current, ...added]);
      this.episodesComplete.set(page.length < EPISODE_PAGE);
    }, isCurrent).finally(() => {
      if (epoch === this.episodeEpoch) {
        this.episodeRequest = null;
        this.loadingEpisodes.set(false);
      }
    });
    this.episodeRequest = { more, promise };
    return promise;
  }

  private invalidateEpisodes(): void {
    this.episodeEpoch++;
    this.episodeRequest = null;
    this.loadingEpisodes.set(false);
  }

  async saveFact(input: FactInput): Promise<Fact | null> {
    let saved: Fact | null = null;
    await this.run(async () => {
      saved = await this.api.saveFact(input);
      if (this.destroyRef.destroyed) return;
      this.listEpochs.facts++;
      const fact = saved;
      this.facts.update((list) => [fact, ...list.filter((entry) => entry.id !== fact.id)]);
      this.clearSearch();
      await Promise.all([this.loadFacts(), this.loadProjects()]);
    });
    return saved;
  }

  deleteFact(id: string): Promise<void> {
    return this.run(async () => {
      await this.api.deleteFact(id);
      if (this.destroyRef.destroyed) return;
      this.listEpochs.facts++;
      this.facts.update((list) => list.filter((f) => f.id !== id));
      this.clearSearch();
    });
  }

  togglePinned(fact: Fact): Promise<Fact | null> {
    return this.saveFact({
      id: fact.id,
      content: fact.content,
      projectId: fact.projectId,
      pinned: !fact.pinned,
      sourceConversationId: fact.sourceConversationId,
    });
  }

  async saveProject(input: ProjectInput): Promise<Project | null> {
    let saved: Project | null = null;
    await this.run(async () => {
      saved = await this.api.saveProject(input);
      if (this.destroyRef.destroyed) return;
      this.listEpochs.projects++;
      const project = saved;
      this.projects.update((list) => [project, ...list.filter((entry) => entry.id !== project.id)]);
      this.clearSearch();
      await this.loadProjects();
    });
    return saved;
  }

  deleteProject(id: string): Promise<void> {
    return this.run(async () => {
      await this.api.deleteProject(id);
      if (this.destroyRef.destroyed) return;
      this.listEpochs.projects++;
      this.listEpochs.facts++;
      this.projects.update((list) => list.filter((project) => project.id !== id));
      this.clearSearch();
      await Promise.all([this.loadProjects(), this.loadFacts()]);
    });
  }

  clearEpisodes(): Promise<void> {
    if (this.clearingHistory) return this.clearingHistory;
    if (this.destroyRef.destroyed || this.dataOperation()) return Promise.resolve();
    this.invalidateEpisodes();
    this.clearingEpisodes.set(true);
    const promise = this.run(async () => {
      await this.api.clearEpisodes();
      if (this.destroyRef.destroyed) return;
      this.episodes.set([]);
      this.episodesComplete.set(true);
      this.clearSearch();
    }).finally(() => {
      this.clearingHistory = null;
      this.clearingEpisodes.set(false);
    });
    this.clearingHistory = promise;
    return promise;
  }

  search(text: string, kinds: MemoryKind[]): Promise<void> {
    if (this.destroyRef.destroyed || this.dataOperation()) return Promise.resolve();
    const query = text.trim();
    const filters = [...new Set(kinds)].sort();
    const key = JSON.stringify([query, filters]);
    if (this.searchRequest?.key === key) return this.searchRequest.promise;
    this.clearSearch();
    if (!query || !filters.length) return Promise.resolve();
    this.searching.set(true);
    const promise = this.readSearch(query, filters, this.searchEpoch);
    this.searchRequest = { key, promise };
    return promise;
  }

  clearSearch(): void {
    this.searchEpoch++;
    this.searchRequest = null;
    this.results.set(null);
    this.searchError.set(null);
    this.searching.set(false);
  }

  private async readSearch(text: string, kinds: MemoryKind[], epoch: number): Promise<void> {
    try {
      const results = await withRequestDeadline(
        () => this.api.searchMemory({ text, kinds, limit: 50 }),
        MEMORY_SEARCH_TIMEOUT_MS,
        'Memory search',
        this.destroyRef
      );
      if (epoch === this.searchEpoch && !this.destroyRef.destroyed) this.results.set(results);
    } catch (error) {
      if (epoch === this.searchEpoch && !this.destroyRef.destroyed) this.searchError.set(IpcError.from(error));
    } finally {
      if (epoch === this.searchEpoch) {
        this.searchRequest = null;
        this.searching.set(false);
      }
    }
  }

  exportMemory(): Promise<void> {
    return this.runDataOperation('export', async () => {
      this.lastTransfer.set(null);
      const report = await this.api.exportMemory();
      if (report) {
        this.lastTransfer.set({ kind: 'export', report });
      }
    });
  }

  importMemory(): Promise<void> {
    return this.runDataOperation('import', async () => {
      this.lastTransfer.set(null);
      const report = await this.api.importMemory();
      if (report) {
        this.lastTransfer.set({ kind: 'import', report });
        this.invalidateEpisodes();
        await this.loadAll();
      }
    });
  }

  /** Deletes all saved memory. The caller has asked the user to confirm. */
  clearAll(): Promise<void> {
    return this.runDataOperation('clear', async () => {
      await this.api.clearMemory(CLEAR_CONFIRMATION);
      this.invalidateEpisodes();
      this.episodes.set([]);
      this.episodesComplete.set(true);
      this.results.set(null);
      this.lastTransfer.set(null);
      await this.loadAll();
    });
  }

  /** One database transfer/clear operation, including its dialog and refresh. */
  private runDataOperation(kind: 'export' | 'import' | 'clear', action: () => Promise<void>): Promise<void> {
    if (this.dataRequest) {
      return this.dataRequest.kind === kind ? this.dataRequest.promise : Promise.resolve();
    }
    // Match disabled UI controls even for callers outside the component.
    if (this.busy()) return Promise.resolve();
    if (kind !== 'export') this.clearSearch();
    this.dataOperation.set(kind);
    this.dataError.set(null);
    this.error.set(null);
    this.pending++;
    this.busy.set(true);
    const promise = action()
      .catch((error: unknown) => this.dataError.set(IpcError.from(error)))
      .finally(() => {
        this.dataRequest = null;
        this.dataOperation.set(null);
        this.pending--;
        this.busy.set(this.pending > 0);
      });
    this.dataRequest = { kind, promise };
    return promise;
  }

  /** Runs an action, tracking errors; nested actions share one busy state. */
  private async run(action: () => Promise<void>, isCurrent: () => boolean = () => true): Promise<void> {
    if (this.destroyRef.destroyed) return;
    if (this.pending === 0) this.error.set(null);
    this.pending++;
    this.busy.set(true);
    try {
      await action();
    } catch (error) {
      if (isCurrent() && !this.destroyRef.destroyed) this.error.set(IpcError.from(error));
    } finally {
      this.pending--;
      this.busy.set(this.pending > 0);
    }
  }
}

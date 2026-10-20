import { Injectable, computed, inject, signal } from '@angular/core';
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

/** Phrase the core requires to delete all memory (see `memory_commands.rs`). */
export const CLEAR_CONFIRMATION = 'DELETE ALL MEMORY';
export const EPISODE_PAGE = 100;
export const MAX_FACT_LENGTH = 2000;

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
  private pending = 0;

  readonly status = signal<MemoryStatus | null>(null);
  readonly facts = signal<readonly Fact[]>([]);
  readonly projects = signal<readonly Project[]>([]);
  readonly episodes = signal<readonly Episode[]>([]);
  readonly episodesComplete = signal(false);
  readonly results = signal<readonly SearchHit[] | null>(null);
  readonly error = signal<IpcError | null>(null);
  readonly busy = signal(false);
  readonly lastTransfer = signal<{ kind: 'export' | 'import'; report: TransferReport } | null>(
    null,
  );

  readonly projectNames = computed(
    () => new Map(this.projects().map((p) => [p.id, p.name] as const)),
  );

  async loadAll(): Promise<void> {
    await Promise.all([
      this.refreshStatus(),
      this.loadFacts(),
      this.loadProjects(),
      this.loadEpisodes(),
    ]);
  }

  refreshStatus(): Promise<void> {
    return this.run(async () => this.status.set(await this.api.getMemoryStatus()));
  }

  loadFacts(): Promise<void> {
    return this.run(async () => this.facts.set(await this.api.listFacts()));
  }

  loadProjects(): Promise<void> {
    return this.run(async () => this.projects.set(await this.api.listProjects()));
  }

  /** First page, or the next one with `more`. */
  loadEpisodes(more = false): Promise<void> {
    return this.run(async () => {
      const current = more ? this.episodes() : [];
      const before = more && current.length ? current[current.length - 1].occurredAtMs : null;
      const page = await this.api.listEpisodes(EPISODE_PAGE, before);
      this.episodes.set([...current, ...page]);
      this.episodesComplete.set(page.length < EPISODE_PAGE);
    });
  }

  async saveFact(input: FactInput): Promise<Fact | null> {
    let saved: Fact | null = null;
    await this.run(async () => {
      saved = await this.api.saveFact(input);
      await Promise.all([this.loadFacts(), this.loadProjects()]);
    });
    return saved;
  }

  deleteFact(id: string): Promise<void> {
    return this.run(async () => {
      await this.api.deleteFact(id);
      this.facts.update((list) => list.filter((f) => f.id !== id));
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
      await this.loadProjects();
    });
    return saved;
  }

  deleteProject(id: string): Promise<void> {
    return this.run(async () => {
      await this.api.deleteProject(id);
      await Promise.all([this.loadProjects(), this.loadFacts()]);
    });
  }

  clearEpisodes(): Promise<void> {
    return this.run(async () => {
      await this.api.clearEpisodes();
      this.episodes.set([]);
      this.episodesComplete.set(true);
    });
  }

  search(text: string, kinds: MemoryKind[]): Promise<void> {
    const query = text.trim();
    if (!query) {
      this.results.set(null);
      return Promise.resolve();
    }
    return this.run(async () =>
      this.results.set(await this.api.searchMemory({ text: query, kinds, limit: 50 })),
    );
  }

  exportMemory(): Promise<void> {
    return this.run(async () => {
      const report = await this.api.exportMemory();
      if (report) {
        this.lastTransfer.set({ kind: 'export', report });
      }
    });
  }

  importMemory(): Promise<void> {
    return this.run(async () => {
      const report = await this.api.importMemory();
      if (report) {
        this.lastTransfer.set({ kind: 'import', report });
        await this.loadAll();
      }
    });
  }

  /** Deletes all saved memory. The caller has asked the user to confirm. */
  clearAll(): Promise<void> {
    return this.run(async () => {
      await this.api.clearMemory(CLEAR_CONFIRMATION);
      this.results.set(null);
      await this.loadAll();
    });
  }

  /** Runs an action, tracking errors; nested actions share one busy state. */
  private async run(action: () => Promise<void>): Promise<void> {
    this.pending++;
    this.busy.set(true);
    try {
      await action();
      this.error.set(null);
    } catch (error) {
      this.error.set(IpcError.from(error));
    } finally {
      this.pending--;
      this.busy.set(this.pending > 0);
    }
  }
}

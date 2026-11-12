import { ChangeDetectionStrategy, Component, DestroyRef, computed, inject, signal } from '@angular/core';
import { RouterLink } from '@angular/router';
import type { Fact, FactSuggestion } from '@jarvis/shared-types';

import { MAX_FACT_LENGTH, MemoryStore } from '../../../core/state/memory.store';
import { formatShortDate } from '../../../shared/format';
import { HudPanel } from '../../../shared/hud-panel/hud-panel';
import { Icon } from '../../../shared/icon/icon';

/** `all`, `global` (no project) or a project id. */
type Scope = string;

const SOURCE_LABELS: Record<Fact['source'], string> = {
  user: 'added by you',
  conversation: 'from a chat',
  import: 'imported',
};

/** How the user wants a suggestion saved, before accepting it. */
interface SuggestionDraft {
  readonly content: string;
  readonly pinned: boolean;
  readonly projectId: string | null;
}

/** Long-term facts (semantic memory): review suggestions, add, edit, pin, delete. */
@Component({
  selector: 'jv-facts-panel',
  imports: [HudPanel, Icon, RouterLink],
  templateUrl: './facts-panel.html',
  styleUrls: ['../memory-shared.scss', './facts-panel.scss'],
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class FactsPanel {
  protected readonly store = inject(MemoryStore);
  private readonly destroyRef = inject(DestroyRef);
  private editorRevision = 0;
  protected readonly editorSession = signal(0);
  protected readonly saving = signal(false);
  protected readonly maxLength = MAX_FACT_LENGTH;
  protected readonly formatDate = formatShortDate;
  protected readonly sourceLabel = (fact: Fact): string => SOURCE_LABELS[fact.source];

  protected readonly editingId = signal<string | null>(null);
  protected readonly content = signal('');
  protected readonly projectId = signal<string | null>(null);
  protected readonly pinned = signal(false);
  protected readonly scope = signal<Scope>('all');
  protected readonly filter = signal('');
  protected readonly confirmDeleteId = signal<string | null>(null);
  /** Changes to suggestions, by id; untouched ones are saved as suggested. */
  private readonly drafts = signal<ReadonlyMap<string, SuggestionDraft>>(new Map());

  protected readonly visible = computed(() => {
    const scope = this.scope();
    const text = this.filter().trim().toLowerCase();
    return this.store
      .facts()
      .filter((fact) => (scope === 'all' ? true : scope === 'global' ? !fact.projectId : fact.projectId === scope))
      .filter((fact) => !text || fact.content.toLowerCase().includes(text));
  });
  protected readonly canSave = computed(() => {
    const length = this.content().trim().length;
    return length > 0 && length <= MAX_FACT_LENGTH && !this.store.busy() && !this.saving();
  });

  protected projectName(id: string | null): string {
    return id ? (this.store.projectNames().get(id) ?? 'Project') : 'Global';
  }

  protected edit(fact: Fact): void {
    if (this.destroyRef.destroyed || !this.store.facts().includes(fact)) return;
    this.editorSession.update((session) => session + 1);
    this.editingId.set(fact.id);
    this.content.set(fact.content);
    this.projectId.set(fact.projectId);
    this.pinned.set(fact.pinned);
  }

  protected reset(): void {
    this.editorSession.update((session) => session + 1);
    this.editingId.set(null);
    this.content.set('');
    this.pinned.set(false);
  }

  protected onContentInput(event: Event): void {
    ++this.editorRevision;
    this.content.set(this.valueOf(event));
  }

  protected onProjectChange(event: Event): void {
    ++this.editorRevision;
    this.projectId.set(this.valueOf(event) || null);
  }

  protected onPinnedChange(event: Event): void {
    ++this.editorRevision;
    this.pinned.set(this.checkedOf(event));
  }

  protected async save(session: number): Promise<void> {
    if (this.destroyRef.destroyed || session !== this.editorSession() || !this.canSave()) {
      return;
    }
    const id = this.editingId();
    const existing = this.store.facts().find((f) => f.id === id);
    if (id !== null && !existing) return;
    const revision = this.editorRevision;
    this.saving.set(true);
    try {
      const saved = await this.store.saveFact({
        id,
        content: this.content().trim(),
        projectId: this.projectId(),
        pinned: this.pinned(),
        sourceConversationId: existing?.sourceConversationId ?? null,
      });
      if (!saved || this.destroyRef.destroyed || session !== this.editorSession()) return;
      if (revision === this.editorRevision) this.reset();
      // Further edits belong to the fact just created, so saving them must
      // update that fact instead of creating a second record.
      else if (id === null) this.editingId.set(saved.id);
    } finally {
      this.saving.set(false);
    }
  }

  protected async remove(fact: Fact): Promise<void> {
    if (this.destroyRef.destroyed || this.store.busy() || this.saving() || !this.store.facts().includes(fact)) return;
    const session = this.editorSession();
    const revision = this.editorRevision;
    this.confirmDeleteId.set(null);
    const removed = await this.store.deleteFact(fact.id);
    if (removed && !this.destroyRef.destroyed && session === this.editorSession() && this.editingId() === fact.id) {
      if (revision === this.editorRevision) this.reset();
      else {
        // The edited draft outlived its deleted record. Keep it available
        // as a new fact instead of silently targeting a missing ID.
        this.editingId.set(null);
        this.editorSession.update((current) => current + 1);
      }
    }
  }

  protected togglePinned(fact: Fact): void {
    if (this.destroyRef.destroyed || this.store.busy() || this.saving() || !this.store.facts().includes(fact)) return;
    void this.store.togglePinned(fact);
  }

  protected draftOf(suggestion: FactSuggestion): SuggestionDraft {
    return this.drafts().get(suggestion.id) ?? { content: suggestion.content, pinned: false, projectId: suggestion.projectId };
  }

  protected patchDraft(suggestion: FactSuggestion, patch: Partial<SuggestionDraft>): void {
    const next = { ...this.draftOf(suggestion), ...patch };
    this.drafts.update((drafts) => new Map(drafts).set(suggestion.id, next));
  }

  protected async accept(suggestion: FactSuggestion): Promise<void> {
    const draft = this.draftOf(suggestion);
    const content = draft.content.trim();
    if (!content || content.length > MAX_FACT_LENGTH) {
      return;
    }
    await this.store.acceptSuggestion({ id: suggestion.id, content, pinned: draft.pinned, projectId: draft.projectId });
  }

  protected reject(suggestion: FactSuggestion): void {
    void this.store.rejectSuggestion(suggestion.id);
  }

  protected valueOf(event: Event): string {
    return (event.target as HTMLInputElement).value;
  }

  protected checkedOf(event: Event): boolean {
    return (event.target as HTMLInputElement).checked;
  }
}

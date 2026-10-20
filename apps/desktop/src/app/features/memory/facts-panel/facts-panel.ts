import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import type { Fact } from '@jarvis/shared-types';

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

/** Long-term facts (semantic memory): add, edit, pin, delete. */
@Component({
  selector: 'jv-facts-panel',
  imports: [HudPanel, Icon],
  templateUrl: './facts-panel.html',
  styleUrls: ['../memory-shared.scss', './facts-panel.scss'],
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class FactsPanel {
  protected readonly store = inject(MemoryStore);
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

  protected readonly visible = computed(() => {
    const scope = this.scope();
    const text = this.filter().trim().toLowerCase();
    return this.store
      .facts()
      .filter((fact) =>
        scope === 'all' ? true : scope === 'global' ? !fact.projectId : fact.projectId === scope,
      )
      .filter((fact) => !text || fact.content.toLowerCase().includes(text));
  });
  protected readonly canSave = computed(() => {
    const length = this.content().trim().length;
    return length > 0 && length <= MAX_FACT_LENGTH && !this.store.busy();
  });

  protected projectName(id: string | null): string {
    return id ? (this.store.projectNames().get(id) ?? 'Project') : 'Global';
  }

  protected edit(fact: Fact): void {
    this.editingId.set(fact.id);
    this.content.set(fact.content);
    this.projectId.set(fact.projectId);
    this.pinned.set(fact.pinned);
  }

  protected reset(): void {
    this.editingId.set(null);
    this.content.set('');
    this.pinned.set(false);
  }

  protected async save(): Promise<void> {
    if (!this.canSave()) {
      return;
    }
    const existing = this.store.facts().find((f) => f.id === this.editingId());
    const saved = await this.store.saveFact({
      id: this.editingId(),
      content: this.content().trim(),
      projectId: this.projectId(),
      pinned: this.pinned(),
      sourceConversationId: existing?.sourceConversationId ?? null,
    });
    if (saved) {
      this.reset();
    }
  }

  protected async remove(fact: Fact): Promise<void> {
    this.confirmDeleteId.set(null);
    await this.store.deleteFact(fact.id);
    if (this.editingId() === fact.id) {
      this.reset();
    }
  }

  protected valueOf(event: Event): string {
    return (event.target as HTMLInputElement).value;
  }

  protected checkedOf(event: Event): boolean {
    return (event.target as HTMLInputElement).checked;
  }
}

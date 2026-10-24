import { ChangeDetectionStrategy, Component, DestroyRef, inject, signal } from '@angular/core';
import { Router, RouterLink } from '@angular/router';
import type { MemoryKind, SearchHit } from '@jarvis/shared-types';

import { MemoryStore, snippetParts } from '../../../core/state/memory.store';
import { formatShortDate } from '../../../shared/format';
import { HudPanel } from '../../../shared/hud-panel/hud-panel';
import { Icon } from '../../../shared/icon/icon';

const KINDS: readonly { id: MemoryKind; label: string }[] = [
  { id: 'message', label: 'Messages' },
  { id: 'fact', label: 'Facts' },
  { id: 'episode', label: 'Actions' },
];

/** Full-text and semantic search across memory. */
@Component({
  selector: 'jv-search-panel',
  imports: [HudPanel, Icon, RouterLink],
  templateUrl: './search-panel.html',
  styleUrls: ['../memory-shared.scss', './search-panel.scss'],
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class SearchPanel {
  protected readonly store = inject(MemoryStore);
  private readonly router = inject(Router);

  protected readonly kinds = KINDS;
  protected readonly parts = snippetParts;
  protected readonly formatDate = formatShortDate;
  protected readonly text = signal('');
  protected readonly selected = signal<ReadonlySet<MemoryKind>>(new Set(KINDS.map((k) => k.id)));

  constructor() {
    inject(DestroyRef).onDestroy(() => this.store.clearSearch());
  }

  protected onText(value: string): void {
    this.text.set(value);
    this.store.clearSearch();
  }

  protected toggle(kind: MemoryKind, on: boolean): void {
    this.store.clearSearch();
    this.selected.update((current) => {
      const next = new Set(current);
      if (on) {
        next.add(kind);
      } else {
        next.delete(kind);
      }
      return next;
    });
  }

  protected search(): void {
    void this.store.search(this.text(), [...this.selected()]);
  }

  protected label(hit: SearchHit): string {
    switch (hit.kind) {
      case 'message':
        return `${hit.role === 'assistant' ? 'JARVIS' : 'You'} in “${hit.conversationTitle ?? 'a chat'}”`;
      case 'fact':
        return 'Fact';
      case 'episode':
        return 'Action';
    }
  }

  protected open(hit: SearchHit): void {
    if (hit.kind === 'message' && hit.conversationId) {
      void this.router.navigate(['/chat'], { queryParams: { c: hit.conversationId } });
    }
  }

  protected valueOf(event: Event): string {
    return (event.target as HTMLInputElement).value;
  }

  protected checkedOf(event: Event): boolean {
    return (event.target as HTMLInputElement).checked;
  }
}

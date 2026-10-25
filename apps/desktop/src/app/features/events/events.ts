import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import type { EventCategory, EventLevel } from '@jarvis/shared-types';

import { EventLogStore } from '../../core/state/event-log.store';
import { formatClock } from '../../shared/format';
import { HudPanel } from '../../shared/hud-panel/hud-panel';
import { StatusBadge } from '../../shared/status-badge/status-badge';

const LEVEL_RANK: Record<EventLevel, number> = { debug: 0, info: 1, warning: 2, error: 3 };

@Component({
  selector: 'jv-events',
  imports: [HudPanel, StatusBadge],
  templateUrl: './events.html',
  styleUrl: './events.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Events {
  protected readonly store = inject(EventLogStore);

  protected readonly formatClock = formatClock;
  protected readonly levels: readonly EventLevel[] = ['debug', 'info', 'warning', 'error'];
  protected readonly categories: readonly EventCategory[] = [
    'lifecycle',
    'config',
    'health',
    'ai',
    'runtime',
    'memory',
    'chat',
    'agents',
    'security',
    'platform',
    'ui',
    'voice',
    'vision',
    'plugins',
  ];

  protected readonly minLevel = signal<EventLevel>('info');
  protected readonly category = signal<EventCategory | 'all'>('all');
  protected readonly query = signal('');
  protected readonly error = this.store.error;

  protected readonly filtered = computed(() => {
    const minRank = LEVEL_RANK[this.minLevel()];
    const category = this.category();
    const query = this.query().trim().toLowerCase();
    return this.store
      .events()
      .filter(
        (e) =>
          LEVEL_RANK[e.level] >= minRank &&
          (category === 'all' || e.category === category) &&
          (!query || e.message.toLowerCase().includes(query))
      )
      .slice()
      .reverse();
  });

  protected readonly total = computed(() => this.store.events().length);

  protected onLevel(event: Event): void {
    this.minLevel.set((event.target as HTMLSelectElement).value as EventLevel);
  }

  protected onCategory(event: Event): void {
    this.category.set((event.target as HTMLSelectElement).value as EventCategory | 'all');
  }

  protected onQuery(event: Event): void {
    this.query.set((event.target as HTMLInputElement).value);
  }
}

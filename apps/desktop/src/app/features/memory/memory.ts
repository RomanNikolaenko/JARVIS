import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { RouterLink } from '@angular/router';

import { MemoryStore } from '../../core/state/memory.store';
import { Icon, type IconName } from '../../shared/icon/icon';
import { DataPanel } from './data-panel/data-panel';
import { FactsPanel } from './facts-panel/facts-panel';
import { HistoryPanel } from './history-panel/history-panel';
import { ProjectsPanel } from './projects-panel/projects-panel';
import { SearchPanel } from './search-panel/search-panel';

type Tab = 'facts' | 'projects' | 'history' | 'search' | 'data';

interface TabItem {
  readonly id: Tab;
  readonly label: string;
  readonly icon: IconName;
}

/** Memory screen: facts, projects, the action history, search and data. */
@Component({
  selector: 'jv-memory',
  imports: [RouterLink, Icon, FactsPanel, ProjectsPanel, HistoryPanel, SearchPanel, DataPanel],
  templateUrl: './memory.html',
  styleUrl: './memory.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class MemoryPage {
  protected readonly store = inject(MemoryStore);
  protected readonly tab = signal<Tab>('facts');
  protected readonly tabs: readonly TabItem[] = [
    { id: 'facts', label: 'Facts', icon: 'brain' },
    { id: 'projects', label: 'Projects', icon: 'folder' },
    { id: 'history', label: 'Action history', icon: 'clock' },
    { id: 'search', label: 'Search', icon: 'search' },
    { id: 'data', label: 'Data & settings', icon: 'settings' },
  ];
  protected readonly summary = computed(() => {
    const counts = this.store.status()?.counts;
    if (!counts) {
      return '';
    }
    return `${counts.conversations} conversations · ${counts.messages} messages · ${counts.facts} facts · ${counts.projects} projects`;
  });

  constructor() {
    void this.store.loadAll();
  }
}

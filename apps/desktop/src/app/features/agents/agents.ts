import { ChangeDetectionStrategy, Component, inject, signal } from '@angular/core';

import { AgentsStore } from '../../core/state/agents.store';
import { Icon, type IconName } from '../../shared/icon/icon';
import { AuditPanel } from './audit-panel/audit-panel';
import { TasksPanel } from './tasks-panel/tasks-panel';
import { ToolsPanel } from './tools-panel/tools-panel';

type Tab = 'tasks' | 'tools' | 'audit';

interface TabItem {
  readonly id: Tab;
  readonly label: string;
  readonly icon: IconName;
}

/** Agents screen: tasks, tools and permissions, and the audit log. */
@Component({
  selector: 'jv-agents',
  imports: [Icon, TasksPanel, ToolsPanel, AuditPanel],
  templateUrl: './agents.html',
  styleUrl: './agents.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class AgentsPage {
  protected readonly store = inject(AgentsStore);
  protected readonly tab = signal<Tab>('tasks');
  protected readonly tabs: readonly TabItem[] = [
    { id: 'tasks', label: 'Tasks', icon: 'agent' },
    { id: 'tools', label: 'Tools & permissions', icon: 'shield' },
    { id: 'audit', label: 'Audit log', icon: 'events' },
  ];

  constructor() {
    void this.store.start();
    void this.store.loadAll();
  }
}

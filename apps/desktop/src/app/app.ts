import { ChangeDetectionStrategy, Component, DestroyRef, computed, inject, signal } from '@angular/core';
import { RouterLink, RouterLinkActive, RouterOutlet } from '@angular/router';

import { AgentsStore } from './core/state/agents.store';
import { ChatStore } from './core/state/chat.store';
import { EventLogStore } from './core/state/event-log.store';
import { pollWhileVisible } from './core/state/poll';
import { SettingsStore } from './core/state/settings.store';
import { SystemStore } from './core/state/system.store';
import { ApprovalDialog } from './shared/approval-dialog/approval-dialog';
import { Icon, type IconName } from './shared/icon/icon';
import { StatusBadge } from './shared/status-badge/status-badge';

interface NavItem {
  readonly path: string;
  readonly label: string;
  readonly icon: IconName;
}

const HEALTH_POLL_MS = 15_000;
const CLOCK_TICK_MS = 10_000;

@Component({
  selector: 'jv-root',
  imports: [RouterOutlet, RouterLink, RouterLinkActive, Icon, StatusBadge, ApprovalDialog],
  templateUrl: './app.html',
  styleUrl: './app.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { '[class.jv-reduced-motion]': 'reducedMotion()' },
})
export class App {
  private readonly system = inject(SystemStore);
  private readonly settings = inject(SettingsStore);
  private readonly now = signal(Date.now());

  protected readonly nav: readonly NavItem[] = [
    { path: '/dashboard', label: 'Dashboard', icon: 'dashboard' },
    { path: '/chat', label: 'Chat', icon: 'chat' },
    { path: '/models', label: 'Models', icon: 'spark' },
    { path: '/runtime', label: 'Runtime', icon: 'cpu' },
    { path: '/agents', label: 'Agents', icon: 'agent' },
    { path: '/memory', label: 'Memory', icon: 'brain' },
    { path: '/events', label: 'Event log', icon: 'events' },
    { path: '/settings', label: 'Settings', icon: 'settings' },
  ];

  protected readonly status = this.system.status;
  protected readonly appInfo = this.system.appInfo;
  protected readonly connectionError = this.system.connectionError;
  protected readonly reducedMotion = this.settings.reducedMotion;
  protected readonly statusLabel = computed(() => {
    switch (this.status()) {
      case 'healthy':
        return 'Online';
      case 'degraded':
        return 'Limited';
      case 'unhealthy':
        return 'Faulty';
      case 'offline':
        return 'Core offline';
      default:
        return 'Connecting';
    }
  });
  protected readonly clock = computed(() =>
    new Date(this.now()).toLocaleTimeString(undefined, {
      hour: '2-digit',
      minute: '2-digit',
      hour12: false,
    })
  );

  constructor() {
    const destroyRef = inject(DestroyRef);
    const chat = inject(ChatStore);

    void this.system.loadStaticInfo();
    void inject(EventLogStore).start();
    // Approval requests must reach the user on every screen.
    void inject(AgentsStore).start();
    void this.settings.load().then(() => chat.loadModels());
    pollWhileVisible(() => this.system.refreshHealth(), HEALTH_POLL_MS, destroyRef);
    pollWhileVisible(() => this.now.set(Date.now()), CLOCK_TICK_MS, destroyRef);
  }
}

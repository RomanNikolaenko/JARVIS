import { ChangeDetectionStrategy, Component, DestroyRef, computed, inject, signal } from '@angular/core';
import { RouterLink, RouterLinkActive, RouterOutlet } from '@angular/router';

import { AgentsStore } from './core/state/agents.store';
import { ChatStore } from './core/state/chat.store';
import { EventLogStore } from './core/state/event-log.store';
import { pollWhileVisible } from './core/state/poll';
import { SettingsStore } from './core/state/settings.store';
import { SystemStore } from './core/state/system.store';
import { UpdatesStore } from './core/state/updates.store';
import { VoiceStore } from './core/state/voice.store';
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
  host: {
    '[class.jv-reduced-motion]': 'reducedMotion()',
    '[class.jv-background]': 'background()',
  },
})
export class App {
  private readonly system = inject(SystemStore);
  private readonly settings = inject(SettingsStore);
  private readonly now = signal(Date.now());
  /** Another window has the focus or JARVIS is minimized: decorative animations pause. */
  protected readonly background = signal(inBackground());
  protected readonly voice = inject(VoiceStore);
  protected readonly updates = inject(UpdatesStore);

  protected readonly nav: readonly NavItem[] = [
    { path: '/dashboard', label: 'Dashboard', icon: 'dashboard' },
    { path: '/chat', label: 'Chat', icon: 'chat' },
    { path: '/models', label: 'Models', icon: 'spark' },
    { path: '/runtime', label: 'Runtime', icon: 'cpu' },
    { path: '/agents', label: 'Agents', icon: 'agent' },
    { path: '/memory', label: 'Memory', icon: 'brain' },
    { path: '/plugins', label: 'Plugins', icon: 'plug' },
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
    const eventLog = inject(EventLogStore);
    void eventLog.start();
    // Approval requests must reach the user on every screen.
    const agents = inject(AgentsStore);
    void agents.start();
    // The microphone indicator and spoken requests work on every screen.
    void this.voice.start();
    // A found update is announced on every screen.
    void this.updates.start();
    void this.settings.load().then(() => chat.loadModels());
    pollWhileVisible(
      async () => {
        await this.system.refreshHealth();
        if (destroyRef.destroyed) return;
        const error = this.system.connectionError();
        if (error) {
          agents.connectionLost(error);
          eventLog.connectionLost(error);
          chat.connectionLost(error);
        } else {
          void agents.start();
          void eventLog.start();
        }
      },
      HEALTH_POLL_MS,
      destroyRef
    );
    pollWhileVisible(() => this.now.set(Date.now()), CLOCK_TICK_MS, destroyRef);

    const updateBackground = () => this.background.set(inBackground());
    window.addEventListener('focus', updateBackground);
    window.addEventListener('blur', updateBackground);
    document.addEventListener('visibilitychange', updateBackground);
    destroyRef.onDestroy(() => {
      window.removeEventListener('focus', updateBackground);
      window.removeEventListener('blur', updateBackground);
      document.removeEventListener('visibilitychange', updateBackground);
    });
  }
}

function inBackground(): boolean {
  return document.visibilityState === 'hidden' || !document.hasFocus();
}

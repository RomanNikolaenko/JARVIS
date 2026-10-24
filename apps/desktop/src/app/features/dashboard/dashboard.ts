import { ChangeDetectionStrategy, Component, DestroyRef, computed, inject } from '@angular/core';
import { RouterLink } from '@angular/router';
import type { DataLocality } from '@jarvis/shared-types';

import { ChatStore } from '../../core/state/chat.store';
import { EventLogStore } from '../../core/state/event-log.store';
import { ModelsStore } from '../../core/state/models.store';
import { pollWhileVisible } from '../../core/state/poll';
import { SystemStore } from '../../core/state/system.store';
import { AiCore, type CoreState } from '../../shared/ai-core/ai-core';
import { formatBytes, formatClock, formatPercent, formatUptime } from '../../shared/format';
import { HudPanel } from '../../shared/hud-panel/hud-panel';
import { Icon } from '../../shared/icon/icon';
import { StatusBadge } from '../../shared/status-badge/status-badge';

const METRICS_POLL_MS = 2_000;
const RECENT_EVENT_COUNT = 6;

@Component({
  selector: 'jv-dashboard',
  imports: [RouterLink, AiCore, HudPanel, Icon, StatusBadge],
  templateUrl: './dashboard.html',
  styleUrl: './dashboard.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Dashboard {
  private readonly system = inject(SystemStore);
  private readonly chat = inject(ChatStore);
  private readonly eventLog = inject(EventLogStore);
  private readonly models = inject(ModelsStore);

  protected readonly formatBytes = formatBytes;
  protected readonly formatPercent = formatPercent;
  protected readonly formatUptime = formatUptime;
  protected readonly formatClock = formatClock;
  protected readonly localityLabels: Record<DataLocality, string> = {
    onDevice: 'runs on this device',
    localNetwork: 'local network',
    thirdPartyCloud: 'cloud',
  };

  protected readonly status = this.system.status;
  protected readonly health = this.system.health;
  protected readonly metrics = this.system.metrics;
  protected readonly metricsError = this.system.metricsError;
  protected readonly platform = this.system.platform;
  protected readonly diagnosticOnly = this.chat.diagnosticOnly;
  protected readonly generalRoute = this.models.generalRoute;
  /** Model the router currently uses for general chat. */
  protected readonly activeModel = computed(() => {
    const route = this.generalRoute();
    if (!route?.providerId || !route.model) {
      return null;
    }
    const option = this.models.options().find((o) => o.provider.id === route.providerId && o.model.id === route.model);
    return option ? { ...option, automatic: route.automatic } : null;
  });
  protected readonly chatPhase = this.chat.phase;

  protected readonly coreState = computed<CoreState>(() => {
    const status = this.status();
    if (status === 'offline') {
      return 'offline';
    }
    switch (this.chatPhase()) {
      case 'connecting':
        return 'thinking';
      case 'streaming':
        return 'speaking';
      default:
        return status === 'degraded' || status === 'unhealthy' ? 'degraded' : 'idle';
    }
  });

  protected readonly headline = computed(() => {
    switch (this.status()) {
      case 'healthy':
        return 'All systems operational';
      case 'degraded':
        return 'Operating with limitations';
      case 'unhealthy':
        return 'A core component is failing';
      case 'offline':
        return 'Core offline';
      default:
        return 'Connecting to the core…';
    }
  });

  protected readonly memoryPercent = computed(() => {
    const m = this.metrics();
    return m && m.memoryTotalBytes > 0 ? (m.memoryUsedBytes / m.memoryTotalBytes) * 100 : null;
  });

  protected readonly recentEvents = computed(() => this.eventLog.events().slice(-RECENT_EVENT_COUNT).reverse());

  constructor() {
    pollWhileVisible(() => this.system.refreshMetrics(), METRICS_POLL_MS, inject(DestroyRef));
  }

  protected meterWidth(percent: number | null | undefined): string {
    return `${Math.min(100, Math.max(0, percent ?? 0))}%`;
  }
}

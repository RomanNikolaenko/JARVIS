import { Injectable, computed, inject, signal } from '@angular/core';
import type { AppInfo, HealthReport, HealthStatus, PlatformInfo, SystemMetrics } from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';

/** Overall state shown in the UI. `offline` means the core is unreachable. */
export type JarvisStatus = HealthStatus | 'offline' | 'unknown';

@Injectable({ providedIn: 'root' })
export class SystemStore {
  private readonly api = inject(JarvisApi);
  private healthInFlight = false;
  private metricsInFlight = false;

  readonly appInfo = signal<AppInfo | null>(null);
  readonly platform = signal<PlatformInfo | null>(null);
  readonly health = signal<HealthReport | null>(null);
  readonly metrics = signal<SystemMetrics | null>(null);
  /** Set when the core cannot be reached or the last health check failed. */
  readonly connectionError = signal<IpcError | null>(null);

  readonly status = computed<JarvisStatus>(() => {
    if (this.connectionError()) {
      return 'offline';
    }
    return this.health()?.status ?? 'unknown';
  });

  async loadStaticInfo(): Promise<void> {
    try {
      const [appInfo, platform] = await Promise.all([this.api.getAppInfo(), this.api.getPlatformInfo()]);
      this.appInfo.set(appInfo);
      this.platform.set(platform);
    } catch (error) {
      this.connectionError.set(IpcError.from(error));
    }
  }

  async refreshHealth(): Promise<void> {
    if (this.healthInFlight) {
      return;
    }
    this.healthInFlight = true;
    try {
      this.health.set(await this.api.getHealth());
      this.connectionError.set(null);
    } catch (error) {
      this.connectionError.set(IpcError.from(error));
    } finally {
      this.healthInFlight = false;
    }
  }

  async refreshMetrics(): Promise<void> {
    if (this.metricsInFlight) {
      return;
    }
    this.metricsInFlight = true;
    try {
      this.metrics.set(await this.api.getSystemMetrics());
    } catch (error) {
      this.connectionError.set(IpcError.from(error));
    } finally {
      this.metricsInFlight = false;
    }
  }
}

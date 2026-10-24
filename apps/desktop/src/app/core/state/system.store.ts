import { DestroyRef, Injectable, computed, inject, signal } from '@angular/core';
import type { AppInfo, HealthReport, HealthStatus, PlatformInfo, SystemMetrics } from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';
import { withRequestDeadline } from '../ipc/request-deadline';

/** Overall state shown in the UI. `offline` means the core is unreachable. */
export type JarvisStatus = HealthStatus | 'offline' | 'unknown';

// Core probes run concurrently with a maximum budget of six seconds.
export const HEALTH_REQUEST_TIMEOUT_MS = 10_000;
export const METRICS_REQUEST_TIMEOUT_MS = 10_000;

@Injectable({ providedIn: 'root' })
export class SystemStore {
  private readonly api = inject(JarvisApi);
  private readonly destroyRef = inject(DestroyRef);
  private healthInFlight: Promise<void> | null = null;
  private metricsInFlight: Promise<void> | null = null;

  readonly appInfo = signal<AppInfo | null>(null);
  readonly platform = signal<PlatformInfo | null>(null);
  readonly health = signal<HealthReport | null>(null);
  readonly metrics = signal<SystemMetrics | null>(null);
  readonly metricsError = signal<IpcError | null>(null);
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

  refreshHealth(): Promise<void> {
    if (this.destroyRef.destroyed) return Promise.resolve();
    if (this.healthInFlight) return this.healthInFlight;
    this.healthInFlight = this.probeHealth().finally(() => {
      this.healthInFlight = null;
    });
    return this.healthInFlight;
  }

  private async probeHealth(): Promise<void> {
    try {
      const report = await withRequestDeadline(
        () => this.api.getHealth(),
        HEALTH_REQUEST_TIMEOUT_MS,
        'The core health check',
        this.destroyRef
      );
      if (this.destroyRef.destroyed) return;
      this.health.set(report);
      this.connectionError.set(null);
    } catch (error) {
      if (!this.destroyRef.destroyed) this.connectionError.set(IpcError.from(error));
    }
  }

  refreshMetrics(): Promise<void> {
    if (this.destroyRef.destroyed) return Promise.resolve();
    if (this.metricsInFlight) return this.metricsInFlight;
    this.metricsInFlight = this.sampleMetrics().finally(() => {
      this.metricsInFlight = null;
    });
    return this.metricsInFlight;
  }

  private async sampleMetrics(): Promise<void> {
    try {
      const sample = await withRequestDeadline(
        () => this.api.getSystemMetrics(),
        METRICS_REQUEST_TIMEOUT_MS,
        'Resource monitoring',
        this.destroyRef
      );
      if (this.destroyRef.destroyed) return;
      this.metrics.set(sample);
      this.metricsError.set(null);
    } catch (error) {
      if (!this.destroyRef.destroyed) {
        this.metrics.set(null);
        this.metricsError.set(IpcError.from(error));
      }
    }
  }
}

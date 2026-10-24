import { TestBed } from '@angular/core/testing';
import type { HealthReport, SystemMetrics } from '@jarvis/shared-types';

import { JarvisApi } from '../ipc/jarvis-api';
import { HEALTH_REQUEST_TIMEOUT_MS, METRICS_REQUEST_TIMEOUT_MS, SystemStore } from './system.store';

const HEALTHY: HealthReport = { status: 'healthy', version: 'test', uptimeSecs: 1, checkedAtMs: 1, components: [] };
const METRICS: SystemMetrics = {
  cpuUsagePercent: 10,
  memoryUsedBytes: 100,
  memoryTotalBytes: 200,
  processMemoryBytes: null,
  processCpuPercent: null,
  uptimeSecs: 1,
  sampledAtMs: 1,
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

describe('SystemStore request deadlines', () => {
  let store: SystemStore;
  let getHealth: ReturnType<typeof vi.fn<() => Promise<HealthReport>>>;
  let getSystemMetrics: ReturnType<typeof vi.fn<() => Promise<SystemMetrics>>>;

  beforeEach(() => {
    vi.useFakeTimers();
    getHealth = vi.fn<() => Promise<HealthReport>>().mockResolvedValue(HEALTHY);
    getSystemMetrics = vi.fn<() => Promise<SystemMetrics>>().mockResolvedValue(METRICS);
    TestBed.configureTestingModule({ providers: [{ provide: JarvisApi, useValue: { getHealth, getSystemMetrics } }] });
    store = TestBed.inject(SystemStore);
  });

  afterEach(() => {
    TestBed.resetTestingModule();
    vi.useRealTimers();
  });

  it('shares the pending result between concurrent callers', async () => {
    const response = deferred<HealthReport>();
    getHealth.mockReturnValueOnce(response.promise);
    const first = store.refreshHealth();
    const second = store.refreshHealth();
    expect(second).toBe(first);
    expect(getHealth).toHaveBeenCalledTimes(1);
    response.resolve(HEALTHY);
    await second;
    expect(store.status()).toBe('healthy');
    expect(vi.getTimerCount()).toBe(0);
  });

  it('times out a hung call, permits retry and ignores its late success', async () => {
    const response = deferred<HealthReport>();
    getHealth.mockReturnValueOnce(response.promise);
    const checking = store.refreshHealth();
    await vi.advanceTimersByTimeAsync(HEALTH_REQUEST_TIMEOUT_MS);
    await checking;
    expect(store.status()).toBe('offline');
    expect(store.connectionError()?.code).toBe('timeout');
    const fresh = { ...HEALTHY, checkedAtMs: 2, status: 'degraded' as const };
    getHealth.mockResolvedValueOnce(fresh);
    await store.refreshHealth();
    response.resolve(HEALTHY);
    await vi.advanceTimersByTimeAsync(0);
    expect(store.health()).toEqual(fresh);
    expect(store.status()).toBe('degraded');
    expect(store.connectionError()).toBeNull();
  });

  it('handles a rejection after timeout without corrupting a recovered connection', async () => {
    const response = deferred<HealthReport>();
    getHealth.mockReturnValueOnce(response.promise);
    const checking = store.refreshHealth();
    await vi.advanceTimersByTimeAsync(HEALTH_REQUEST_TIMEOUT_MS);
    await checking;
    await store.refreshHealth();
    response.reject(new Error('Late native failure'));
    await vi.advanceTimersByTimeAsync(0);
    expect(store.status()).toBe('healthy');
    expect(store.connectionError()).toBeNull();
  });

  it('clears its timer on immediate native failure and permits retry', async () => {
    getHealth.mockRejectedValueOnce({ code: 'unavailable', message: 'Core stopped' });
    await store.refreshHealth();
    expect(store.connectionError()?.code).toBe('unavailable');
    expect(vi.getTimerCount()).toBe(0);
    await store.refreshHealth();
    expect(store.status()).toBe('healthy');
  });

  it('settles on destruction and never publishes late state', async () => {
    const response = deferred<HealthReport>();
    getHealth.mockReturnValueOnce(response.promise);
    const checking = store.refreshHealth();
    TestBed.resetTestingModule();
    await checking;
    expect(vi.getTimerCount()).toBe(0);
    response.resolve(HEALTHY);
    await vi.advanceTimersByTimeAsync(0);
    await store.refreshHealth();
    expect(store.health()).toBeNull();
    expect(store.connectionError()).toBeNull();
    expect(getHealth).toHaveBeenCalledTimes(1);
  });

  it('shares a metrics request without blocking health checks', async () => {
    const response = deferred<SystemMetrics>();
    getSystemMetrics.mockReturnValueOnce(response.promise);
    const first = store.refreshMetrics();
    expect(store.refreshMetrics()).toBe(first);
    await store.refreshHealth();
    expect(store.status()).toBe('healthy');
    response.resolve(METRICS);
    await first;
    expect(getSystemMetrics).toHaveBeenCalledTimes(1);
    expect(store.metrics()).toEqual(METRICS);
    expect(vi.getTimerCount()).toBe(0);
  });

  it.each(['success', 'failure'])('ignores late metrics %s after timeout and recovery', async (outcome) => {
    await store.refreshHealth();
    await store.refreshMetrics();
    const response = deferred<SystemMetrics>();
    getSystemMetrics.mockReturnValueOnce(response.promise);
    const pending = store.refreshMetrics();
    await vi.advanceTimersByTimeAsync(METRICS_REQUEST_TIMEOUT_MS);
    await pending;
    expect(store.metrics()).toBeNull();
    expect(store.metricsError()?.code).toBe('timeout');
    expect(store.status()).toBe('healthy');
    const fresh = { ...METRICS, sampledAtMs: 2 };
    getSystemMetrics.mockResolvedValueOnce(fresh);
    await store.refreshMetrics();
    if (outcome === 'success') response.resolve(METRICS);
    else response.reject(new Error('Late metrics error'));
    await vi.advanceTimersByTimeAsync(0);
    expect(store.metrics()).toEqual(fresh);
    expect(store.metricsError()).toBeNull();
    expect(store.connectionError()).toBeNull();
  });

  it('keeps a metrics failure local and does not clear a health failure on metrics success', async () => {
    await store.refreshHealth();
    getSystemMetrics.mockRejectedValueOnce({ code: 'internal', message: 'Sensor unavailable' });
    await store.refreshMetrics();
    expect(store.status()).toBe('healthy');
    expect(store.metricsError()?.message).toBe('Sensor unavailable');
    getHealth.mockRejectedValueOnce({ code: 'unavailable', message: 'Core offline' });
    await store.refreshHealth();
    await store.refreshMetrics();
    expect(store.metricsError()).toBeNull();
    expect(store.status()).toBe('offline');
    expect(vi.getTimerCount()).toBe(0);
  });

  it('settles a metrics request on destruction and discards its late sample', async () => {
    const response = deferred<SystemMetrics>();
    getSystemMetrics.mockReturnValueOnce(response.promise);
    const pending = store.refreshMetrics();
    TestBed.resetTestingModule();
    await pending;
    response.resolve(METRICS);
    await vi.advanceTimersByTimeAsync(0);
    await store.refreshMetrics();
    expect(store.metrics()).toBeNull();
    expect(store.metricsError()).toBeNull();
    expect(getSystemMetrics).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(0);
  });
});

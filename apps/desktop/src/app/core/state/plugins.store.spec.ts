import { TestBed } from '@angular/core/testing';
import type { PluginInfo } from '@jarvis/shared-types';

import { pluginInfo, pluginReview } from '../../testing/plugins.fixture';
import { JarvisApi } from '../ipc/jarvis-api';
import { PluginsStore } from './plugins.store';

describe('PluginsStore', () => {
  let api: Record<string, ReturnType<typeof vi.fn>>;
  let pushed: ((plugins: PluginInfo[]) => void) | undefined;
  let store: PluginsStore;

  beforeEach(() => {
    pushed = undefined;
    api = {
      onPlugins: vi.fn(async (handler: (plugins: PluginInfo[]) => void) => {
        pushed = handler;
        return () => undefined;
      }),
      listPlugins: vi.fn(async () => [pluginInfo()]),
      pickPluginFolder: vi.fn(async () => null),
      reviewMcpServer: vi.fn(async () => pluginReview()),
      installPlugin: vi.fn(async () => pluginInfo({ id: 'my-server', name: 'My server', status: { state: 'starting' } })),
      setPluginTool: vi.fn(async () => pluginInfo({ tools: [] })),
      removePlugin: vi.fn(async () => undefined),
      getPluginLogs: vi.fn(async () => ['line 1']),
    };
    TestBed.configureTestingModule({ providers: [{ provide: JarvisApi, useValue: api }] });
    store = TestBed.inject(PluginsStore);
  });

  it('loads plugins and follows pushed changes', async () => {
    await store.start();
    expect(store.loaded()).toBe(true);
    expect(store.plugins().map((p) => p.id)).toEqual(['notes']);
    pushed?.([pluginInfo({ status: { state: 'disabled' }, enabled: false })]);
    expect(store.plugins()[0].status.state).toBe('disabled');
  });

  it('installs only a confirmed review', async () => {
    await store.start();
    await store.pickFolder();
    expect(store.review()).toBeNull();

    await store.reviewServer({ name: 'My server', program: 'npx', args: [], cwd: null, env: [], secretNames: [] });
    expect(store.review()?.reviewId).toBe('review-1');
    expect(api['installPlugin']).not.toHaveBeenCalled();

    expect(await store.confirmReview()).toBe(true);
    expect(api['installPlugin']).toHaveBeenCalledWith('review-1');
    expect(store.review()).toBeNull();
    expect(store.plugins().map((p) => p.id)).toEqual(['notes', 'my-server']);
  });

  it('keeps the review and reports when the plugin changed meanwhile', async () => {
    api['installPlugin'].mockRejectedValueOnce({
      code: 'permissionDenied',
      message: 'The plugin changed after the review. Review it again.',
    });
    await store.reviewServer({ name: 'x', program: 'x', args: [], cwd: null, env: [], secretNames: [] });
    expect(await store.confirmReview()).toBe(false);
    expect(store.review()).not.toBeNull();
    expect(store.error()?.code).toBe('permissionDenied');
  });

  it('sends tool choices and removes plugins', async () => {
    await store.start();
    await store.setTool('notes', 'search', false, 'high');
    expect(api['setPluginTool']).toHaveBeenCalledWith({ pluginId: 'notes', tool: 'search', enabled: false, risk: 'high' });
    expect(store.plugins()[0].tools).toEqual([]);
    await store.loadLogs('notes');
    expect(store.logs()['notes']).toEqual(['line 1']);
    await store.remove('notes');
    expect(store.plugins()).toEqual([]);
  });
  it('preserves events newer than the initial snapshot', async () => {
    let resolve!: (value: PluginInfo[]) => void;
    api['listPlugins'].mockReturnValueOnce(
      new Promise((done) => {
        resolve = done;
      })
    );
    const start = store.start();
    await Promise.resolve();
    pushed?.([pluginInfo({ enabled: false, status: { state: 'disabled' } })]);
    resolve([pluginInfo()]);
    await start;
    expect(store.plugins()[0].enabled).toBe(false);
  });
  it('cleans up partial startup before retrying and on destruction', async () => {
    const stop = vi.fn();
    api['onPlugins'].mockResolvedValue(stop);
    api['listPlugins'].mockRejectedValueOnce(new Error('offline'));
    await store.start();
    expect(stop).toHaveBeenCalledTimes(1);
    await store.start();
    TestBed.resetTestingModule();
    expect(stop).toHaveBeenCalledTimes(2);
  });
  it('releases a subscription that resolves after destruction', async () => {
    let resolve!: (value: () => void) => void;
    const stop = vi.fn();
    api['onPlugins'].mockReturnValueOnce(
      new Promise((done) => {
        resolve = done;
      })
    );
    const start = store.start();
    TestBed.resetTestingModule();
    resolve(stop);
    await start;
    expect(stop).toHaveBeenCalledTimes(1);
    expect(api['listPlugins']).not.toHaveBeenCalled();
  });
  it('retries a timed out registration and disposes the late old listener', async () => {
    vi.useFakeTimers();
    try {
      let finish!: (value: () => void) => void;
      let oldHandler!: (value: PluginInfo[]) => void;
      const stop = vi.fn();
      api['onPlugins'].mockImplementationOnce((handler: (value: PluginInfo[]) => void) => {
        oldHandler = handler;
        return new Promise((resolve) => {
          finish = resolve;
        });
      });
      const first = store.start();
      await vi.advanceTimersByTimeAsync(10_001);
      await first;
      expect(store.error()?.code).toBe('timeout');
      await store.start();
      const current = store.plugins();
      oldHandler([]);
      expect(store.plugins()).toEqual(current);
      finish(stop);
      await Promise.resolve();
      expect(stop).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });
});

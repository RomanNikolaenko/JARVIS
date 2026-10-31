import { TestBed } from '@angular/core/testing';
import type { ModelCapabilities, ProviderModels, ProviderTestResult, RouteResolution, Settings } from '@jarvis/shared-types';

import { JarvisApi } from '../ipc/jarvis-api';
import { MODEL_SUBSCRIPTION_TIMEOUT_MS, MODEL_TEST_TIMEOUT_MS, ModelsStore, supportsCategory } from './models.store';
import { IpcError } from '../ipc/ipc-error';
import { SETTINGS } from '../../testing/settings.fixture';
import { SettingsStore } from './settings.store';

const CHAT: ModelCapabilities = {
  chat: true,
  streaming: true,
  embeddings: false,
  toolCalling: false,
  vision: false,
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((yes) => {
    resolve = yes;
  });
  return { promise, resolve };
}

function provider(id: string, kind: 'diagnostic' | 'localServer', error: string | null) {
  return {
    provider: { id, displayName: id, kind, locality: 'onDevice' },
    models: error
      ? []
      : [
          {
            id: `${id}-model`,
            displayName: 'M',
            providerId: id,
            contextWindow: null,
            capabilities: CHAT,
          },
        ],
    error,
  } satisfies ProviderModels;
}

describe('ModelsStore', () => {
  let api: {
    listAiModels: ReturnType<typeof vi.fn>;
    getModelRoutes: ReturnType<typeof vi.fn>;
    updateSettings: ReturnType<typeof vi.fn>;
    onModelsChanged: ReturnType<typeof vi.fn>;
    testAiProvider: ReturnType<typeof vi.fn>;
  };
  let store: ModelsStore;

  beforeEach(() => {
    api = {
      listAiModels: vi.fn(async () => [
        provider('diagnostic-echo', 'diagnostic', null),
        provider('ollama', 'localServer', null),
        provider('lm-studio', 'localServer', 'cannot connect'),
      ]),
      getModelRoutes: vi.fn(async (): Promise<RouteResolution[]> => []),
      updateSettings: vi.fn(async (s: Settings) => s),
      onModelsChanged: vi.fn(async (_handler: () => void) => () => undefined),
      testAiProvider: vi.fn(async (): Promise<ProviderTestResult> => ({
        reachable: true,
        locality: 'onDevice',
        latencyMs: 1,
        models: [],
        error: null,
      })),
    };
    TestBed.configureTestingModule({ providers: [{ provide: JarvisApi, useValue: api }] });
    store = TestBed.inject(ModelsStore);
    TestBed.inject(SettingsStore).settings.set(SETTINGS);
  });

  it('exposes options only from reachable providers', async () => {
    await store.refresh();
    expect(store.loaded()).toBe(true);
    expect(store.options().map((o) => o.provider.id)).toEqual(['diagnostic-echo', 'ollama']);
    expect(store.realModelAvailable()).toBe(true);
  });

  it.each([true, false])('keeps a provider workflow exclusive through its final refresh (%s)', async (succeeded) => {
    const complete = deferred<boolean>();
    const first = store.withProviderMutation(() => complete.promise);
    const conflicting = vi.fn(async () => true);
    expect(store.providerMutationPending()).toBe(true);
    // Another page instance receives the same root store while the first page's
    // asynchronous settings/key workflow still owns the guard.
    expect(await TestBed.inject(ModelsStore).withProviderMutation(conflicting)).toBe(false);
    expect(conflicting).not.toHaveBeenCalled();
    complete.resolve(succeeded);
    expect(await first).toBe(succeeded);
    expect(store.providerMutationPending()).toBe(false);
    expect(await store.withProviderMutation(conflicting)).toBe(true);
    expect(conflicting).toHaveBeenCalledOnce();
  });

  it('releases the provider guard after an error and refuses writes during a shared settings save or disposal', async () => {
    await expect(
      store.withProviderMutation(async () => {
        throw new Error('Vault failure');
      })
    ).rejects.toThrow('Vault failure');
    expect(store.providerMutationPending()).toBe(false);
    const settingsWrite = deferred<Settings>();
    api.updateSettings.mockReturnValueOnce(settingsWrite.promise);
    const saving = TestBed.inject(SettingsStore).save(SETTINGS);
    const action = vi.fn(async () => true);
    expect(await store.withProviderMutation(action)).toBe(false);
    settingsWrite.resolve(SETTINGS);
    await saving;
    expect(await store.withProviderMutation(action)).toBe(true);
    TestBed.resetTestingModule();
    expect(await store.withProviderMutation(action)).toBe(false);
    expect(action).toHaveBeenCalledOnce();
  });

  it('times out a stalled provider test and permits a new test before the old native call finishes', async () => {
    vi.useFakeTimers();
    try {
      const old = deferred<ProviderTestResult>();
      api.testAiProvider.mockReturnValueOnce(old.promise);
      const config = SETTINGS.ai.providers[0];
      const first = store.testProvider(config, 'test-only-key');
      // Attach the assertion before advancing timers to handle the rejection.
      const timedOut = expect(first).rejects.toMatchObject({ code: 'timeout' });
      await vi.advanceTimersByTimeAsync(MODEL_TEST_TIMEOUT_MS);
      await timedOut;
      expect(api.testAiProvider).toHaveBeenCalledWith(config, 'test-only-key');
      const current = await store.testProvider(config);
      expect(current.reachable).toBe(true);
      old.resolve({ ...current, reachable: false, error: 'Old native result' });
      await vi.advanceTimersByTimeAsync(0);
      expect(current.reachable).toBe(true);
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it('settles provider testing when the application is disposed and refuses another native call', async () => {
    vi.useFakeTimers();
    try {
      api.testAiProvider.mockReturnValueOnce(new Promise(() => undefined));
      const config = SETTINGS.ai.providers[0];
      const pending = store.testProvider(config);
      const disposed = expect(pending).rejects.toMatchObject({ code: 'cancelled' });
      TestBed.resetTestingModule();
      await disposed;
      await expect(store.testProvider(config)).rejects.toMatchObject({ code: 'cancelled' });
      expect(api.testAiProvider).toHaveBeenCalledOnce();
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it('shares concurrent refreshes and forces a reload on demand', async () => {
    await Promise.all([store.refresh(), store.refresh()]);
    expect(api.listAiModels).toHaveBeenCalledTimes(1);
    await store.refresh(true);
    expect(api.listAiModels).toHaveBeenLastCalledWith(true);
  });

  it('resolves routing only after the forced model catalog is refreshed', async () => {
    const catalog = deferred<ProviderModels[]>();
    let activeModel = 'old-model';
    api.listAiModels.mockReturnValueOnce(catalog.promise);
    api.getModelRoutes.mockImplementation(async () => [
      {
        category: 'general',
        providerId: 'new',
        model: activeModel,
        automatic: true,
        error: null,
      },
    ]);
    const refreshing = store.refresh(true);
    expect(api.getModelRoutes).not.toHaveBeenCalled();
    activeModel = 'new-model';
    catalog.resolve([provider('new', 'localServer', null)]);
    await refreshing;
    expect(store.chatOptions()[0].model.id).toBe('new-model');
    expect(store.generalRoute()?.model).toBe('new-model');
    expect(api.getModelRoutes).toHaveBeenCalledOnce();
  });

  it('uses one deadline across catalog and route requests and ignores late routes', async () => {
    vi.useFakeTimers();
    try {
      const catalog = deferred<ProviderModels[]>();
      const routes = deferred<RouteResolution[]>();
      api.listAiModels.mockReturnValueOnce(catalog.promise);
      api.getModelRoutes.mockReturnValueOnce(routes.promise);
      const refreshing = store.refresh(true);
      await vi.advanceTimersByTimeAsync(20_000);
      catalog.resolve([provider('new', 'localServer', null)]);
      await vi.advanceTimersByTimeAsync(0);
      expect(api.getModelRoutes).toHaveBeenCalledOnce();
      await vi.advanceTimersByTimeAsync(10_000);
      await refreshing;
      expect(store.error()?.code).toBe('timeout');
      expect(store.loading()).toBe(false);
      routes.resolve([]);
      await vi.advanceTimersByTimeAsync(0);
      expect(store.providers()).toEqual([]);
      expect(store.loaded()).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });

  it('releases a late catalog without starting another request after destruction', async () => {
    const catalog = deferred<ProviderModels[]>();
    api.listAiModels.mockReturnValueOnce(catalog.promise);
    const refreshing = store.refresh(true);
    TestBed.resetTestingModule();
    await refreshing;
    catalog.resolve([provider('old', 'localServer', null)]);
    await Promise.resolve();
    expect(api.getModelRoutes).not.toHaveBeenCalled();
    expect(store.providers()).toEqual([]);
    expect(store.error()).toBeNull();
  });

  it('subscribes once and refreshes the catalog and routes when models change', async () => {
    let changed!: () => void;
    api.onModelsChanged.mockImplementation(async (handler: () => void) => {
      changed = handler;
      return () => undefined;
    });
    const first = store.start();
    expect(store.start()).toBe(first);
    await first;
    await store.start();
    expect(api.onModelsChanged).toHaveBeenCalledOnce();
    expect(api.listAiModels).toHaveBeenCalledTimes(1);
    api.listAiModels.mockResolvedValueOnce([provider('embedded', 'localServer', null)]);
    changed();
    await store.refresh();
    expect(api.listAiModels).toHaveBeenCalledTimes(2);
    expect(api.listAiModels).toHaveBeenLastCalledWith(true);
    expect(api.getModelRoutes).toHaveBeenCalledTimes(2);
    expect(store.providers()[0].provider.id).toBe('embedded');
  });

  it('retries failed subscriptions without a successful refresh hiding the subscription error', async () => {
    api.onModelsChanged.mockRejectedValueOnce(new Error('Listener unavailable'));
    await store.start();
    await store.refresh();
    expect(store.subscriptionError()?.message).toBe('Listener unavailable');
    expect(store.error()?.message).toBe('Listener unavailable');
    await store.start();
    expect(api.onModelsChanged).toHaveBeenCalledTimes(2);
    expect(store.subscriptionError()).toBeNull();
    expect(store.error()).toBeNull();
  });

  it('times out registration, retries, and ignores the old handler and late cleanup', async () => {
    vi.useFakeTimers();
    try {
      const registration = deferred<() => void>();
      const cleanup = vi.fn();
      let staleChanged!: () => void;
      api.onModelsChanged.mockImplementationOnce((handler: () => void) => {
        staleChanged = handler;
        return registration.promise;
      });
      const first = store.start();
      await vi.advanceTimersByTimeAsync(MODEL_SUBSCRIPTION_TIMEOUT_MS);
      await first;
      expect(store.subscriptionError()?.code).toBe('timeout');
      expect(store.connecting()).toBe(false);
      await store.start();
      expect(store.subscriptionError()).toBeNull();
      staleChanged();
      registration.resolve(cleanup);
      await vi.advanceTimersByTimeAsync(0);
      expect(cleanup).toHaveBeenCalledOnce();
      expect(api.listAiModels).toHaveBeenCalledOnce();
      TestBed.resetTestingModule();
      expect(cleanup).toHaveBeenCalledOnce();
    } finally {
      vi.useRealTimers();
    }
  });

  it('releases a listener registered after destruction without loading models', async () => {
    const registration = deferred<() => void>();
    const cleanup = vi.fn();
    let changed!: () => void;
    api.onModelsChanged.mockImplementationOnce((handler: () => void) => {
      changed = handler;
      return registration.promise;
    });
    const starting = store.start();
    TestBed.resetTestingModule();
    await starting;
    changed();
    registration.resolve(cleanup);
    await registration.promise;
    expect(cleanup).toHaveBeenCalledOnce();
    expect(api.listAiModels).not.toHaveBeenCalled();
    expect(store.subscriptionError()).toBeNull();
    expect(store.connecting()).toBe(false);
  });

  it('reconnects while registration is pending and ignores a stale listener and catalog', async () => {
    const registration = deferred<() => void>();
    const oldCatalog = deferred<ProviderModels[]>();
    const cleanup = vi.fn();
    let staleChanged!: () => void;
    let changed!: () => void;
    api.onModelsChanged
      .mockImplementationOnce((handler: () => void) => {
        staleChanged = handler;
        return registration.promise;
      })
      .mockImplementationOnce(async (handler: () => void) => {
        changed = handler;
        return () => undefined;
      });
    api.listAiModels.mockReturnValueOnce(oldCatalog.promise);
    const oldRefresh = store.refresh();
    const first = store.start();
    store.connectionLost(new IpcError('unavailable', 'Disconnected'));
    await store.start();
    staleChanged();
    oldCatalog.resolve([provider('old', 'localServer', null)]);
    registration.resolve(cleanup);
    await Promise.all([oldRefresh, first]);
    expect(cleanup).toHaveBeenCalledOnce();
    expect(store.providers()[0].provider.id).toBe('diagnostic-echo');
    expect(store.subscriptionError()).toBeNull();
    changed();
    await store.refresh();
    expect(api.listAiModels).toHaveBeenCalledTimes(3);
    expect(api.getModelRoutes).toHaveBeenCalledTimes(2);
  });

  it('keeps a model-change refresh when an older routing save finishes afterward', async () => {
    let changed!: () => void;
    api.onModelsChanged.mockImplementation(async (handler: () => void) => {
      changed = handler;
      return () => undefined;
    });
    await store.start();
    const oldRoutes = deferred<RouteResolution[]>();
    const oldRoute: RouteResolution = {
      category: 'general',
      providerId: 'old',
      model: 'old-model',
      automatic: true,
      error: null,
    };
    const newRoute: RouteResolution = { ...oldRoute, providerId: 'new', model: 'new-model' };
    api.listAiModels
      .mockResolvedValueOnce([provider('old', 'localServer', null)])
      .mockResolvedValueOnce([provider('new', 'localServer', null)]);
    api.getModelRoutes.mockReturnValueOnce(oldRoutes.promise).mockResolvedValueOnce([newRoute]);
    const saving = store.saveRouting({ ...SETTINGS.ai.routing, general: null });
    await vi.waitFor(() => expect(api.getModelRoutes).toHaveBeenCalledTimes(2));
    changed();
    await store.refresh();
    expect(store.generalRoute()).toEqual(newRoute);
    oldRoutes.resolve([oldRoute]);
    expect(await saving).toBe(true);
    expect(store.generalRoute()).toEqual(newRoute);
    expect(store.chatOptions()[0].model.id).toBe('new-model');
  });

  it('saves providers and routing in one settings update', async () => {
    const routing = { ...SETTINGS.ai.routing, general: null };
    await store.saveProviders([], routing);
    const saved = api.updateSettings.mock.calls[0]?.[0] as Settings;
    expect(saved.ai.providers).toEqual([]);
    expect(saved.ai.routing.general).toBeNull();
    expect(api.listAiModels).toHaveBeenCalledWith(true);
  });

  it('mirrors the core capability rules', () => {
    expect(supportsCategory('general', CHAT)).toBe(true);
    expect(supportsCategory('vision', CHAT)).toBe(false);
    expect(supportsCategory('vision', { ...CHAT, vision: true })).toBe(true);
    expect(supportsCategory('embeddings', { ...CHAT, chat: false, embeddings: true })).toBe(true);
    expect(supportsCategory('toolCalling', CHAT)).toBe(false);
  });
  it('keeps the forced refresh when an older refresh finishes last', async () => {
    let old!: (value: ProviderModels[]) => void;
    api.listAiModels.mockReturnValueOnce(
      new Promise((resolve) => {
        old = resolve;
      })
    );
    const first = store.refresh();
    api.listAiModels.mockResolvedValueOnce([provider('new', 'localServer', null)]);
    await store.refresh(true);
    old([provider('old', 'localServer', null)]);
    await first;
    expect(store.providers()[0].provider.id).toBe('new');
  });
  it('does not clear loading or deduplication while a newer refresh is pending', async () => {
    let old!: (value: ProviderModels[]) => void;
    let next!: (value: ProviderModels[]) => void;
    api.listAiModels.mockReturnValueOnce(
      new Promise((resolve) => {
        old = resolve;
      })
    );
    api.listAiModels.mockReturnValueOnce(
      new Promise((resolve) => {
        next = resolve;
      })
    );
    const first = store.refresh();
    const second = store.refresh(true);
    old([]);
    await first;
    expect(store.loading()).toBe(true);
    expect(store.refresh()).toBe(second);
    next([]);
    await second;
    expect(store.loading()).toBe(false);
  });
  it('can retry a timed out refresh and ignores its late result', async () => {
    vi.useFakeTimers();
    try {
      let old!: (value: ProviderModels[]) => void;
      api.listAiModels.mockReturnValueOnce(
        new Promise((resolve) => {
          old = resolve;
        })
      );
      const first = store.refresh();
      await vi.advanceTimersByTimeAsync(30_001);
      await first;
      expect(store.error()?.code).toBe('timeout');
      expect(store.loading()).toBe(false);
      api.listAiModels.mockResolvedValueOnce([provider('retry', 'localServer', null)]);
      await store.refresh();
      old([provider('old', 'localServer', null)]);
      await Promise.resolve();
      expect(store.providers()[0].provider.id).toBe('retry');
    } finally {
      vi.useRealTimers();
    }
  });
});

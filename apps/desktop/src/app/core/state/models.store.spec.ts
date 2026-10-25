import { TestBed } from '@angular/core/testing';
import type { ModelCapabilities, ProviderModels, RouteResolution, Settings } from '@jarvis/shared-types';

import { JarvisApi } from '../ipc/jarvis-api';
import { ModelsStore, supportsCategory } from './models.store';
import { SETTINGS } from '../../testing/settings.fixture';
import { SettingsStore } from './settings.store';

const CHAT: ModelCapabilities = {
  chat: true,
  streaming: true,
  embeddings: false,
  toolCalling: false,
  vision: false,
};

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

  it('shares concurrent refreshes and forces a reload on demand', async () => {
    await Promise.all([store.refresh(), store.refresh()]);
    expect(api.listAiModels).toHaveBeenCalledTimes(1);
    await store.refresh(true);
    expect(api.listAiModels).toHaveBeenLastCalledWith(true);
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

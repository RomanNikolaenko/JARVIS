import { TestBed } from '@angular/core/testing';
import type {
  ModelCapabilities,
  ProviderModels,
  RouteResolution,
  Settings,
} from '@jarvis/shared-types';

import { JarvisApi } from '../ipc/jarvis-api';
import { ModelsStore, supportsCategory } from './models.store';
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

const SETTINGS = {
  schemaVersion: 1,
  general: { assistantLanguage: 'uk' },
  interface: { reducedMotion: false },
  ai: {
    providers: [
      {
        id: 'ollama',
        backend: 'ollama',
        displayName: 'Ollama',
        baseUrl: 'http://127.0.0.1:11434',
        enabled: true,
      },
    ],
    routing: {
      general: { providerId: 'ollama', model: 'x' },
      reasoning: null,
      coding: null,
      vision: null,
      embeddings: null,
      toolCalling: null,
    },
    requestTimeoutSecs: 300,
    idleTimeoutSecs: 120,
  },
  privacy: { allowCloudProviders: false },
  logging: { level: 'info' },
} satisfies Settings;

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
});

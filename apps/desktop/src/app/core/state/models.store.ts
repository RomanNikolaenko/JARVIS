import { Injectable, computed, inject, signal } from '@angular/core';
import type {
  ModelCapabilities,
  ModelInfo,
  ModelRouting,
  ProviderConfig,
  ProviderDescriptor,
  ProviderModels,
  ProviderTestResult,
  RouteResolution,
  TaskCategory,
} from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';
import { SettingsStore } from './settings.store';

export interface ModelOption {
  readonly provider: ProviderDescriptor;
  readonly model: ModelInfo;
}

export function optionKey(providerId: string, modelId: string): string {
  return `${providerId}::${modelId}`;
}

/** Mirrors `TaskCategory::supports` in the Rust core. */
export function supportsCategory(category: TaskCategory, caps: ModelCapabilities): boolean {
  switch (category) {
    case 'general':
    case 'reasoning':
    case 'coding':
      return caps.chat;
    case 'vision':
      return caps.chat && caps.vision;
    case 'toolCalling':
      return caps.chat && caps.toolCalling;
    case 'embeddings':
      return caps.embeddings;
  }
}

/** AI providers, their models and the model routing (AI Model Manager). */
@Injectable({ providedIn: 'root' })
export class ModelsStore {
  private readonly api = inject(JarvisApi);
  private readonly settingsStore = inject(SettingsStore);
  private refreshing: Promise<void> | null = null;

  readonly providers = signal<readonly ProviderModels[]>([]);
  readonly routes = signal<readonly RouteResolution[]>([]);
  readonly loading = signal(false);
  readonly loaded = signal(false);
  readonly error = signal<IpcError | null>(null);

  /** Configured providers (including disabled ones) from settings. */
  readonly configured = computed<readonly ProviderConfig[]>(() => this.settingsStore.settings()?.ai.providers ?? []);
  readonly routing = computed<ModelRouting | null>(() => this.settingsStore.settings()?.ai.routing ?? null);

  /** All available models, in provider order. */
  readonly options = computed<ModelOption[]>(() =>
    this.providers()
      .filter((p) => !p.error)
      .flatMap((p) => p.models.map((model) => ({ provider: p.provider, model })))
  );
  readonly chatOptions = computed(() => this.options().filter((o) => o.model.capabilities.chat));
  /** True once loaded and at least one real (non-diagnostic) chat model is reachable. */
  readonly realModelAvailable = computed(() => this.chatOptions().some((o) => o.provider.kind !== 'diagnostic'));
  readonly generalRoute = computed(() => this.routes().find((r) => r.category === 'general') ?? null);

  /** Loads providers and routes; concurrent calls share one request. */
  refresh(force = false): Promise<void> {
    if (this.refreshing && !force) {
      return this.refreshing;
    }
    this.loading.set(true);
    const run = (async () => {
      try {
        const [providers, routes] = await Promise.all([this.api.listAiModels(force), this.api.getModelRoutes()]);
        this.providers.set(providers);
        this.routes.set(routes);
        this.error.set(null);
        this.loaded.set(true);
      } catch (error) {
        this.error.set(IpcError.from(error));
      } finally {
        this.loading.set(false);
        this.refreshing = null;
      }
    })();
    this.refreshing = run;
    return run;
  }

  optionsFor(category: TaskCategory): ModelOption[] {
    return this.options().filter((o) => supportsCategory(category, o.model.capabilities));
  }

  testProvider(config: ProviderConfig, apiKey?: string): Promise<ProviderTestResult> {
    return this.api.testAiProvider(config, apiKey);
  }

  hasApiKey(providerId: string): Promise<boolean> {
    return this.api.hasProviderApiKey(providerId);
  }

  async setApiKey(providerId: string, apiKey: string): Promise<void> {
    await this.api.setProviderApiKey(providerId, apiKey);
    await this.refresh(true);
  }

  async deleteApiKey(providerId: string): Promise<void> {
    await this.api.deleteProviderApiKey(providerId);
    await this.refresh(true);
  }

  /**
   * Persists a new provider list (validated by the core) and reloads models.
   * Pass `routing` when routes referencing removed providers must change in
   * the same update.
   */
  async saveProviders(providers: ProviderConfig[], routing?: ModelRouting): Promise<boolean> {
    const settings = this.settingsStore.settings();
    if (!settings) {
      return false;
    }
    const saved = await this.settingsStore.save({
      ...settings,
      ai: { ...settings.ai, providers, routing: routing ?? settings.ai.routing },
    });
    if (saved) {
      await this.refresh(true);
    }
    return saved;
  }

  async saveRouting(routing: ModelRouting): Promise<boolean> {
    const settings = this.settingsStore.settings();
    if (!settings) {
      return false;
    }
    const saved = await this.settingsStore.save({ ...settings, ai: { ...settings.ai, routing } });
    if (saved) {
      try {
        this.routes.set(await this.api.getModelRoutes());
      } catch (error) {
        this.error.set(IpcError.from(error));
      }
    }
    return saved;
  }
}

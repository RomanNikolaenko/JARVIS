import { DestroyRef, Injectable, computed, inject, signal } from '@angular/core';
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
import { withRequestDeadline } from '../ipc/request-deadline';
import { SettingsStore } from './settings.store';

export const MODEL_SUBSCRIPTION_TIMEOUT_MS = 30_000;
export const MODEL_TEST_TIMEOUT_MS = 30_000;

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
  private readonly destroyRef = inject(DestroyRef);
  private refreshEpoch = 0;
  private started = false;
  private starting: Promise<void> | null = null;
  private disconnect: (() => void) | null = null;
  private readonly requestError = signal<IpcError | null>(null);
  private readonly providerMutation = signal(false);

  readonly providers = signal<readonly ProviderModels[]>([]);
  readonly routes = signal<readonly RouteResolution[]>([]);
  readonly loading = signal(false);
  readonly loaded = signal(false);
  readonly subscriptionError = signal<IpcError | null>(null);
  readonly error = computed(() => this.subscriptionError() ?? this.requestError());
  readonly connecting = signal(false);
  /** Provider settings, key writes and their catalog refresh share one guard. */
  readonly providerMutationPending = this.providerMutation.asReadonly();

  constructor() {
    this.destroyRef.onDestroy(() => this.disconnect?.());
  }

  /** Listen globally before refreshing so autostart and missed changes reach every screen. */
  start(): Promise<void> {
    if (this.destroyRef.destroyed) return Promise.resolve();
    if (this.starting) return this.starting;
    if (this.started) return Promise.resolve();
    this.connecting.set(true);
    const attempt = this.subscribe().finally(() => {
      if (this.starting === attempt) {
        this.starting = null;
        this.connecting.set(false);
      }
    });
    this.starting = attempt;
    return attempt;
  }

  connectionLost(error: IpcError): void {
    if (this.destroyRef.destroyed) return;
    this.disconnect?.();
    this.disconnect = null;
    this.started = false;
    this.starting = null;
    this.connecting.set(false);
    this.subscriptionError.set(error);
    ++this.refreshEpoch;
    this.refreshing = null;
    this.loading.set(false);
  }

  private async subscribe(): Promise<void> {
    let active = true;
    let unlisten: (() => void) | null = null;
    const dispose = () => {
      active = false;
      unlisten?.();
      unlisten = null;
    };
    this.disconnect = dispose;
    try {
      await withRequestDeadline(
        () =>
          this.api
            .onModelsChanged(() => {
              if (active && this.started) void this.refresh(true);
            })
            .then((cleanup) => {
              if (!active) cleanup();
              else unlisten = cleanup;
            }),
        MODEL_SUBSCRIPTION_TIMEOUT_MS,
        'AI model subscription',
        this.destroyRef
      );
      if (!active) return;
      this.started = true;
      this.subscriptionError.set(null);
      await this.refresh(true);
    } catch (error) {
      if (active) this.subscriptionError.set(IpcError.from(error));
      dispose();
    }
  }

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
    if (this.destroyRef.destroyed) return Promise.resolve();
    if (this.refreshing && !force) {
      return this.refreshing;
    }
    this.loading.set(true);
    const epoch = ++this.refreshEpoch;
    let active = true;
    const current = () => active && !this.destroyRef.destroyed && epoch === this.refreshEpoch;
    const run = (async () => {
      try {
        const snapshot = await withRequestDeadline(
          async () => {
            const providers = await this.api.listAiModels(force);
            if (!current()) return null;
            // Routes must resolve against the catalog that was just refreshed.
            const routes = await this.api.getModelRoutes();
            return { providers, routes };
          },
          30_000,
          'AI models and routes',
          this.destroyRef
        );
        if (!current() || !snapshot) return;
        this.providers.set(snapshot.providers);
        this.routes.set(snapshot.routes);
        this.requestError.set(null);
        this.loaded.set(true);
      } catch (error) {
        if (current()) this.requestError.set(IpcError.from(error));
      } finally {
        active = false;
        if (epoch === this.refreshEpoch) {
          this.loading.set(false);
          this.refreshing = null;
        }
      }
    })();
    this.refreshing = run;
    return run;
  }

  optionsFor(category: TaskCategory): ModelOption[] {
    return this.options().filter((o) => supportsCategory(category, o.model.capabilities));
  }

  testProvider(config: ProviderConfig, apiKey?: string): Promise<ProviderTestResult> {
    return withRequestDeadline(
      () => this.api.testAiProvider(config, apiKey),
      MODEL_TEST_TIMEOUT_MS,
      'AI provider connection test',
      this.destroyRef
    );
  }

  /** Keeps the complete provider workflow exclusive even if its page is reopened. */
  async withProviderMutation(action: () => Promise<boolean>): Promise<boolean> {
    if (this.destroyRef.destroyed || this.providerMutation() || this.settingsStore.saving()) return false;
    this.providerMutation.set(true);
    try {
      return await action();
    } finally {
      this.providerMutation.set(false);
    }
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
      await this.refresh(true);
    }
    return saved;
  }
}

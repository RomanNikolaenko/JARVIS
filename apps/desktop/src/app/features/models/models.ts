import { DecimalPipe } from '@angular/common';
import { ChangeDetectionStrategy, Component, DestroyRef, computed, inject, signal } from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { FormControl, FormGroup, FormsModule, ReactiveFormsModule, Validators } from '@angular/forms';
import type {
  DataLocality,
  ModelRef,
  ModelRouting,
  ProviderBackend,
  ProviderConfig,
  ProviderModels,
  ProviderTestResult,
  TaskCategory,
} from '@jarvis/shared-types';

import { IpcError } from '../../core/ipc/ipc-error';
import { ModelsStore, optionKey } from '../../core/state/models.store';
import { SettingsStore } from '../../core/state/settings.store';
import { HudPanel } from '../../shared/hud-panel/hud-panel';
import { Icon } from '../../shared/icon/icon';
import { StatusBadge } from '../../shared/status-badge/status-badge';

export const DEFAULT_URLS: Record<ProviderBackend, string> = {
  ollama: 'http://127.0.0.1:11434',
  openAiCompatible: 'http://127.0.0.1:1234/v1',
  openAi: 'https://api.openai.com/v1',
  anthropic: 'https://api.anthropic.com/v1',
  gemini: 'https://generativelanguage.googleapis.com/v1beta',
};

export const BACKEND_LABELS: Record<ProviderBackend, string> = {
  ollama: 'Ollama',
  openAiCompatible: 'OpenAI-compatible',
  openAi: 'OpenAI',
  anthropic: 'Anthropic',
  gemini: 'Google Gemini',
};

export const LOCALITY_LABELS: Record<DataLocality, string> = {
  onDevice: 'On this device',
  localNetwork: 'Local network',
  thirdPartyCloud: 'Cloud',
};

interface CategoryInfo {
  readonly id: TaskCategory;
  readonly label: string;
  readonly hint: string;
}

const CATEGORIES: readonly CategoryInfo[] = [
  { id: 'general', label: 'General', hint: 'Chat and everyday questions.' },
  { id: 'reasoning', label: 'Reasoning', hint: 'Multi-step problems. Uses General if unset.' },
  { id: 'coding', label: 'Coding', hint: 'Code generation and review. Uses General if unset.' },
  { id: 'vision', label: 'Vision', hint: 'Images and screenshots (vision models only).' },
  { id: 'embeddings', label: 'Embeddings', hint: 'Semantic memory search (Phase 4).' },
  { id: 'toolCalling', label: 'Tool calling', hint: 'Agents and tools (Phase 5).' },
];

const ROUTING_KEYS: Record<TaskCategory, keyof ModelRouting> = {
  general: 'general',
  reasoning: 'reasoning',
  coding: 'coding',
  vision: 'vision',
  embeddings: 'embeddings',
  toolCalling: 'toolCalling',
};

/** Lowercase slug usable as provider id; unique among `taken`. */
export function providerIdFor(name: string, taken: readonly string[]): string {
  const base =
    name
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, '-')
      .replace(/^-+|-+$/g, '')
      .slice(0, 48) || 'provider';
  let id = base;
  for (let n = 2; taken.includes(id) || id === 'diagnostic-echo'; n++) {
    id = `${base}-${n}`;
  }
  return id;
}

@Component({
  selector: 'jv-models',
  imports: [DecimalPipe, FormsModule, ReactiveFormsModule, HudPanel, Icon, StatusBadge],
  templateUrl: './models.html',
  styleUrl: './models.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Models {
  protected readonly store = inject(ModelsStore);
  private readonly settings = inject(SettingsStore);
  private readonly destroyRef = inject(DestroyRef);
  private editorGeneration = 0;
  private testGeneration = 0;

  protected readonly categories = CATEGORIES;
  protected readonly backendLabels = BACKEND_LABELS;
  protected readonly localityLabels = LOCALITY_LABELS;
  protected readonly saveError = this.settings.error;

  /** `null`: form closed; `''`: adding a new provider; otherwise the edited id. */
  protected readonly editingId = signal<string | null>(null);
  protected readonly testing = signal(false);
  protected readonly saving = signal(false);
  protected readonly savingRouting = signal(false);
  protected readonly mutationBusy = computed(
    () => this.saving() || this.savingRouting() || this.settings.saving() || this.store.providerMutationPending()
  );
  protected readonly hasStoredKey = signal(false);
  protected readonly testResult = signal<ProviderTestResult | null>(null);
  protected readonly testError = signal<string | null>(null);
  protected readonly routingDraft = signal<ModelRouting | null>(null);
  protected readonly confirmingRemoval = signal<string | null>(null);

  protected readonly form = new FormGroup({
    nativeToolModels: new FormControl('', {
      nonNullable: true,
      validators: [
        Validators.maxLength(32896),
        (control) => {
          const models = parseNativeToolModels(control.value as string);
          return models.length > 128 ||
            models.some(
              (model) =>
                new TextEncoder().encode(model).length > 256 ||
                Array.from(model).some((character) => {
                  const code = character.charCodeAt(0);
                  return code < 32 || (code >= 127 && code <= 159);
                })
            )
            ? { modelIds: true }
            : null;
        },
      ],
    }),
    apiKey: new FormControl('', {
      nonNullable: true,
      validators: [
        (control) => {
          const key = control.value as string;
          const controls = Array.from(key).some((character) => {
            const code = character.charCodeAt(0);
            return code < 32 || (code >= 127 && code <= 159);
          });
          return (key.length > 0 && !key.trim()) || new TextEncoder().encode(key).length > 8192 || controls
            ? { apiKey: true }
            : null;
        },
      ],
    }),
    displayName: new FormControl('', {
      nonNullable: true,
      validators: [
        (control) => {
          const name = (control.value as string).trim();
          return !name || Array.from(name).length > 64 ? { providerName: true } : null;
        },
      ],
    }),
    backend: new FormControl<ProviderBackend>('ollama', { nonNullable: true }),
    baseUrl: new FormControl(DEFAULT_URLS.ollama, {
      nonNullable: true,
      validators: [Validators.required, Validators.pattern(/^https?:\/\/\S+$/i)],
    }),
  });

  /** Configured providers joined with their live status. */
  protected readonly rows = computed(() => {
    const live = new Map<string, ProviderModels>(this.store.providers().map((p) => [p.provider.id, p]));
    return this.store.configured().map((config) => ({ config, live: live.get(config.id) }));
  });

  protected readonly availableProviders = computed(() => this.store.providers().filter((p) => !p.error && p.models.length > 0));

  protected readonly routing = computed(() => this.routingDraft() ?? this.store.routing());
  protected readonly routingDirty = computed(() => this.routingDraft() !== null);

  constructor() {
    this.form.valueChanges.pipe(takeUntilDestroyed(this.destroyRef)).subscribe(() => this.invalidateTest());
    void this.store.refresh();
  }

  protected refresh(): void {
    void this.store.refresh(true);
  }

  protected statusOf(live: ProviderModels | undefined, enabled: boolean): string {
    if (!enabled) {
      return 'unknown';
    }
    if (!live) {
      return 'unknown';
    }
    return live.error ? 'unhealthy' : 'healthy';
  }

  protected statusLabel(live: ProviderModels | undefined, enabled: boolean): string {
    if (!enabled) {
      return 'Disabled';
    }
    if (!live) {
      return 'Not loaded';
    }
    if (live.error) {
      return 'Unreachable';
    }
    const chat = live.models.filter((m) => m.capabilities.chat).length;
    return `${live.models.length} models · ${chat} chat`;
  }

  // ---- Provider editing ------------------------------------------------

  protected startAdd(): void {
    if (this.mutationBusy() || this.testing()) return;
    ++this.editorGeneration;
    this.invalidateTest();
    this.hasStoredKey.set(false);
    this.editingId.set('');
    this.resetTest();
    this.form.reset({ displayName: '', backend: 'ollama', baseUrl: DEFAULT_URLS.ollama });
  }

  protected async startEdit(config: ProviderConfig): Promise<void> {
    if (this.mutationBusy() || this.testing()) return;
    const editor = ++this.editorGeneration;
    this.invalidateTest();
    this.hasStoredKey.set(false);
    this.editingId.set(config.id);
    this.resetTest();
    this.form.reset({
      displayName: config.displayName,
      backend: config.backend,
      baseUrl: config.baseUrl,
      nativeToolModels: (config.nativeToolModels ?? []).join('\n'),
    });
    const test = this.testGeneration;
    try {
      const present = await this.store.hasApiKey(config.id);
      if (!this.destroyRef.destroyed && this.editorGeneration === editor) this.hasStoredKey.set(present);
    } catch (error) {
      if (!this.destroyRef.destroyed && this.editorGeneration === editor && this.testGeneration === test) {
        this.testError.set(IpcError.from(error).message);
      }
    }
  }

  protected cancelEdit(): void {
    if (this.mutationBusy()) return;
    this.closeEditor();
  }

  private closeEditor(): void {
    ++this.editorGeneration;
    this.invalidateTest();
    this.form.controls.apiKey.reset();
    this.editingId.set(null);
    this.resetTest();
  }

  protected onBackendChange(): void {
    this.invalidateTest();
    const { backend, baseUrl } = this.form.getRawValue();
    if (backend !== 'openAiCompatible' && backend !== 'openAi') {
      this.form.controls.nativeToolModels.reset();
    }
    // Swap the URL only if it is still one of the defaults.
    if (Object.values(DEFAULT_URLS).includes(baseUrl)) {
      this.form.controls.baseUrl.setValue(DEFAULT_URLS[backend]);
    }
    this.resetTest();
  }

  protected async test(): Promise<void> {
    if (this.testing() || this.mutationBusy() || this.editingId() === null) return;
    if (this.form.invalid) {
      this.form.markAllAsTouched();
      return;
    }
    const request = ++this.testGeneration;
    const current = () => !this.destroyRef.destroyed && this.testGeneration === request;
    this.testing.set(true);
    this.resetTest();
    try {
      const result = await this.store.testProvider(this.draftConfig(true), this.form.controls.apiKey.value || undefined);
      if (current()) this.testResult.set(result);
    } catch (error) {
      if (current()) this.testError.set(IpcError.from(error).message);
    } finally {
      if (current()) this.testing.set(false);
    }
  }

  protected async saveProvider(): Promise<void> {
    if (this.mutationBusy() || this.testing()) return;
    if (this.form.invalid) {
      this.form.markAllAsTouched();
      return;
    }
    const editing = this.editingId();
    const draft = this.draftConfig(true);
    const key = this.form.controls.apiKey.value;
    const current = this.store.configured();
    const next =
      editing === '' || editing === null
        ? [...current, draft]
        : current.map((p) => (p.id === editing ? { ...draft, enabled: p.enabled } : p));
    this.saving.set(true);
    this.resetTest();
    try {
      await this.store.withProviderMutation(async () => {
        if (!(await this.store.saveProviders(next))) return false;
        // Retain the saved id on a keychain failure so retrying cannot add a duplicate.
        this.editingId.set(draft.id);
        if (key) await this.store.setApiKey(draft.id, key);
        this.closeEditor();
        return true;
      });
    } catch (error) {
      this.testError.set(IpcError.from(error).message);
    } finally {
      this.saving.set(false);
    }
  }

  protected async deleteKey(): Promise<void> {
    const id = this.editingId();
    if (!id || this.mutationBusy() || this.testing()) return;
    this.saving.set(true);
    this.resetTest();
    try {
      const deleted = await this.store.withProviderMutation(async () => {
        await this.store.deleteApiKey(id);
        return true;
      });
      if (deleted) {
        this.hasStoredKey.set(false);
        this.form.controls.apiKey.reset();
      }
    } catch (error) {
      this.testError.set(IpcError.from(error).message);
    } finally {
      this.saving.set(false);
    }
  }

  protected async toggle(config: ProviderConfig): Promise<void> {
    if (this.mutationBusy() || this.testing()) return;
    this.saving.set(true);
    try {
      await this.store.withProviderMutation(() =>
        this.store.saveProviders(this.store.configured().map((p) => (p.id === config.id ? { ...p, enabled: !p.enabled } : p)))
      );
    } finally {
      this.saving.set(false);
    }
  }

  protected askRemove(config: ProviderConfig): void {
    if (this.mutationBusy() || this.testing()) return;
    this.confirmingRemoval.set(config.id);
  }

  protected async remove(config: ProviderConfig): Promise<void> {
    if (this.mutationBusy() || this.testing()) return;
    this.confirmingRemoval.set(null);
    const routing = this.store.routing();
    this.saving.set(true);
    try {
      const saved = await this.store.withProviderMutation(() =>
        this.store.saveProviders(
          this.store.configured().filter((p) => p.id !== config.id),
          routing ? withoutProvider(routing, config.id) : undefined
        )
      );
      if (saved && this.editingId() === config.id) this.closeEditor();
    } finally {
      this.saving.set(false);
    }
  }

  private draftConfig(enabled: boolean): ProviderConfig {
    const value = this.form.getRawValue();
    const editing = this.editingId();
    const taken = this.store.configured().map((p) => p.id);
    return {
      id: editing ? editing : providerIdFor(value.displayName, taken),
      backend: value.backend,
      displayName: value.displayName.trim(),
      baseUrl: value.baseUrl.trim(),
      enabled,
      nativeToolModels:
        value.backend === 'openAiCompatible' || value.backend === 'openAi' ? parseNativeToolModels(value.nativeToolModels) : [],
    };
  }

  private resetTest(): void {
    this.testResult.set(null);
    this.testError.set(null);
  }

  /** Cancels frontend waiting; the dispatched read-only request may still finish. */
  private invalidateTest(): void {
    ++this.testGeneration;
    this.testing.set(false);
    this.resetTest();
  }

  // ---- Routing -----------------------------------------------------------

  protected optionsFor(category: TaskCategory) {
    return this.store.optionsFor(category);
  }

  protected keyOf(providerId: string, modelId: string): string {
    return optionKey(providerId, modelId);
  }

  protected selectedKey(category: TaskCategory): string {
    const ref = this.routing()?.[ROUTING_KEYS[category]];
    return ref ? optionKey(ref.providerId, ref.model) : '';
  }

  protected routeUnavailable(category: TaskCategory): boolean {
    const key = this.selectedKey(category);
    return key !== '' && !this.optionsFor(category).some((option) => optionKey(option.provider.id, option.model.id) === key);
  }

  protected resolution(category: TaskCategory) {
    return this.store.routes().find((r) => r.category === category);
  }

  protected onRouteChange(category: TaskCategory, event: Event): void {
    const key = (event.target as HTMLSelectElement).value;
    const base = this.routing();
    if (!base) {
      return;
    }
    const option = this.optionsFor(category).find((o) => optionKey(o.provider.id, o.model.id) === key);
    if (key !== '' && !option) return;
    const ref: ModelRef | null = option ? { providerId: option.provider.id, model: option.model.id } : null;
    this.routingDraft.set({ ...base, [ROUTING_KEYS[category]]: ref });
  }

  protected async saveRouting(): Promise<void> {
    if (this.mutationBusy()) return;
    const draft = this.routingDraft();
    if (!draft) return;
    this.savingRouting.set(true);
    try {
      if ((await this.store.saveRouting(draft)) && !this.destroyRef.destroyed && this.routingDraft() === draft) {
        this.routingDraft.set(null);
      }
    } finally {
      this.savingRouting.set(false);
    }
  }

  protected discardRouting(): void {
    if (this.mutationBusy()) return;
    this.routingDraft.set(null);
  }
}

function parseNativeToolModels(value: string): string[] {
  return [
    ...new Set(
      value
        .split(/\r?\n/)
        .map((model) => model.trim())
        .filter(Boolean)
    ),
  ];
}

/** Routing without explicit choices that point at `providerId`. */
export function withoutProvider(routing: ModelRouting, providerId: string): ModelRouting {
  const next = { ...routing };
  for (const key of Object.values(ROUTING_KEYS)) {
    if (next[key]?.providerId === providerId) {
      next[key] = null;
    }
  }
  return next;
}

import { ChangeDetectionStrategy, Component, computed, effect, inject, signal } from '@angular/core';
import { RouterLink } from '@angular/router';
import type { MemorySettings } from '@jarvis/shared-types';

import { MemoryStore } from '../../../core/state/memory.store';
import { type ModelOption, ModelsStore, optionKey } from '../../../core/state/models.store';
import { SettingsStore } from '../../../core/state/settings.store';
import { formatBytes } from '../../../shared/format';
import { HudPanel } from '../../../shared/hud-panel/hud-panel';
import { Icon } from '../../../shared/icon/icon';

/** Word the user types before all memory is deleted. */
export const CLEAR_WORD = 'DELETE';
const AUTOMATIC = '';

/** Database status, memory settings, export/import and deleting everything. */
@Component({
  selector: 'jv-data-panel',
  imports: [HudPanel, Icon, RouterLink],
  templateUrl: './data-panel.html',
  styleUrls: ['../memory-shared.scss', './data-panel.scss'],
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class DataPanel {
  protected readonly store = inject(MemoryStore);
  private readonly settingsStore = inject(SettingsStore);
  private readonly models = inject(ModelsStore);

  protected readonly clearWord = CLEAR_WORD;
  protected readonly formatBytes = formatBytes;
  protected readonly automatic = AUTOMATIC;
  protected readonly keyOf = (option: ModelOption): string => optionKey(option.provider.id, option.model.id);

  protected readonly form = signal<MemorySettings | null>(null);
  protected readonly dirty = signal(false);
  protected readonly saved = signal(false);
  protected readonly confirmation = signal('');
  protected readonly settingsError = this.settingsStore.error;
  protected readonly saving = this.settingsStore.saving;
  protected readonly shareWithCloud = computed(() => this.settingsStore.settings()?.privacy.shareMemoryWithCloud ?? false);
  /** Models that report embeddings first; any other model can be chosen too. */
  protected readonly embeddingOptions = computed(() => {
    const options = this.models.options().filter((o) => o.provider.kind !== 'diagnostic');
    return [
      ...options.filter((o) => o.model.capabilities.embeddings),
      ...options.filter((o) => !o.model.capabilities.embeddings),
    ];
  });
  protected readonly embeddingKey = computed(() => {
    const model = this.form()?.embeddingModel;
    return model ? optionKey(model.providerId, model.model) : AUTOMATIC;
  });

  constructor() {
    void this.models.refresh();
    effect(() => {
      const settings = this.settingsStore.settings();
      if (settings && !this.dirty()) {
        this.form.set({ ...settings.memory });
      }
    });
  }

  protected update(patch: Partial<MemorySettings>): void {
    this.form.update((current) => (current ? { ...current, ...patch } : current));
    this.dirty.set(true);
    this.saved.set(false);
  }

  protected onEmbedding(event: Event): void {
    const key = (event.target as HTMLSelectElement).value;
    const option = this.embeddingOptions().find((o) => this.keyOf(o) === key);
    this.update({
      embeddingModel: option ? { providerId: option.provider.id, model: option.model.id } : null,
    });
  }

  protected onContextLimit(event: Event): void {
    const value = (event.target as HTMLInputElement).value.trim();
    this.update({ contextLimitTokens: value === '' ? null : Number(value) });
  }

  protected async save(): Promise<void> {
    if (this.saving()) return;
    const current = this.settingsStore.settings();
    const memory = this.form();
    if (!current || !memory) {
      return;
    }
    this.saved.set(false);
    if (await this.settingsStore.save({ ...current, memory })) {
      if (JSON.stringify(this.form()) === JSON.stringify(memory)) {
        this.dirty.set(false);
        this.saved.set(true);
      }
      void this.store.refreshStatus();
    }
  }

  protected discard(): void {
    if (this.saving()) return;
    const settings = this.settingsStore.settings();
    this.dirty.set(false);
    this.saved.set(false);
    if (settings) {
      this.form.set({ ...settings.memory });
    }
  }

  protected async clearAll(): Promise<void> {
    if (this.store.busy()) return;
    if (this.confirmation() !== CLEAR_WORD) {
      return;
    }
    this.confirmation.set('');
    await this.store.clearAll();
  }

  protected valueOf(event: Event): string {
    return (event.target as HTMLInputElement).value;
  }

  protected checkedOf(event: Event): boolean {
    return (event.target as HTMLInputElement).checked;
  }
}

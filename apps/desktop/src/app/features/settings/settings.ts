import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
} from '@angular/core';
import { FormControl, FormGroup, ReactiveFormsModule, Validators } from '@angular/forms';
import type { AssistantLanguage, LogLevel, Settings } from '@jarvis/shared-types';

import { ChatStore } from '../../core/state/chat.store';
import { SettingsStore } from '../../core/state/settings.store';
import { SystemStore } from '../../core/state/system.store';
import { HudPanel } from '../../shared/hud-panel/hud-panel';
import { Icon } from '../../shared/icon/icon';

/** Separator between provider id and model id in the model select. */
const KEY_SEPARATOR = '::';

@Component({
  selector: 'jv-settings',
  imports: [ReactiveFormsModule, HudPanel, Icon],
  templateUrl: './settings.html',
  styleUrl: './settings.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class SettingsPage {
  private readonly store = inject(SettingsStore);
  private readonly chat = inject(ChatStore);

  protected readonly appInfo = inject(SystemStore).appInfo;
  protected readonly saving = this.store.saving;
  protected readonly error = this.store.error;
  protected readonly savedAt = signal<number | null>(null);
  protected readonly modelOptions = this.chat.modelOptions;
  protected readonly logLevels: readonly LogLevel[] = ['error', 'warn', 'info', 'debug', 'trace'];

  protected readonly form = new FormGroup({
    assistantLanguage: new FormControl<AssistantLanguage>('uk', { nonNullable: true }),
    reducedMotion: new FormControl(false, { nonNullable: true }),
    defaultModel: new FormControl('', { nonNullable: true }),
    requestTimeoutSecs: new FormControl(300, {
      nonNullable: true,
      validators: [Validators.required, Validators.min(10), Validators.max(3600)],
    }),
    idleTimeoutSecs: new FormControl(60, {
      nonNullable: true,
      validators: [Validators.required, Validators.min(5), Validators.max(600)],
    }),
    allowCloudProviders: new FormControl(false, { nonNullable: true }),
    logLevel: new FormControl<LogLevel>('info', { nonNullable: true }),
  });

  protected readonly loaded = computed(() => this.store.settings() !== null);

  constructor() {
    // Mirror stored settings into the form unless the user is editing.
    effect(() => {
      const settings = this.store.settings();
      if (settings && !this.form.dirty) {
        this.form.reset(toFormValue(settings));
      }
    });
  }

  protected optionKey(providerId: string, modelId: string): string {
    return `${providerId}${KEY_SEPARATOR}${modelId}`;
  }

  protected async save(): Promise<void> {
    const current = this.store.settings();
    if (!current || this.form.invalid) {
      this.form.markAllAsTouched();
      return;
    }
    const value = this.form.getRawValue();
    const [providerId, model] = value.defaultModel ? splitKey(value.defaultModel) : [null, null];

    const next: Settings = {
      ...current,
      general: { ...current.general, assistantLanguage: value.assistantLanguage },
      interface: { ...current.interface, reducedMotion: value.reducedMotion },
      ai: {
        ...current.ai,
        defaultProviderId: providerId,
        defaultModel: model,
        requestTimeoutSecs: value.requestTimeoutSecs,
        idleTimeoutSecs: value.idleTimeoutSecs,
      },
      privacy: { ...current.privacy, allowCloudProviders: value.allowCloudProviders },
      logging: { ...current.logging, level: value.logLevel },
    };

    if (await this.store.save(next)) {
      this.form.markAsPristine();
      this.savedAt.set(Date.now());
      void this.chat.loadModels();
    }
  }

  protected discard(): void {
    const settings = this.store.settings();
    if (settings) {
      this.form.reset(toFormValue(settings));
    }
  }
}

function toFormValue(settings: Settings) {
  const { ai } = settings;
  return {
    assistantLanguage: settings.general.assistantLanguage,
    reducedMotion: settings.interface.reducedMotion,
    defaultModel:
      ai.defaultProviderId && ai.defaultModel
        ? `${ai.defaultProviderId}${KEY_SEPARATOR}${ai.defaultModel}`
        : '',
    requestTimeoutSecs: ai.requestTimeoutSecs,
    idleTimeoutSecs: ai.idleTimeoutSecs,
    allowCloudProviders: settings.privacy.allowCloudProviders,
    logLevel: settings.logging.level,
  };
}

function splitKey(key: string): [string, string] {
  const index = key.indexOf(KEY_SEPARATOR);
  return [key.slice(0, index), key.slice(index + KEY_SEPARATOR.length)];
}

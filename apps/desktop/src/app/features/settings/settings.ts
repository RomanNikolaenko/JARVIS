import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
} from '@angular/core';
import { FormControl, FormGroup, ReactiveFormsModule, Validators } from '@angular/forms';
import { RouterLink } from '@angular/router';
import type { AssistantLanguage, LogLevel, Settings } from '@jarvis/shared-types';

import { SettingsStore } from '../../core/state/settings.store';
import { SystemStore } from '../../core/state/system.store';
import { HudPanel } from '../../shared/hud-panel/hud-panel';
import { Icon } from '../../shared/icon/icon';

@Component({
  selector: 'jv-settings',
  imports: [ReactiveFormsModule, RouterLink, HudPanel, Icon],
  templateUrl: './settings.html',
  styleUrl: './settings.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class SettingsPage {
  private readonly store = inject(SettingsStore);

  protected readonly appInfo = inject(SystemStore).appInfo;
  protected readonly saving = this.store.saving;
  protected readonly error = this.store.error;
  protected readonly savedAt = signal<number | null>(null);
  protected readonly logLevels: readonly LogLevel[] = ['error', 'warn', 'info', 'debug', 'trace'];

  protected readonly form = new FormGroup({
    assistantLanguage: new FormControl<AssistantLanguage>('uk', { nonNullable: true }),
    reducedMotion: new FormControl(false, { nonNullable: true }),
    requestTimeoutSecs: new FormControl(300, {
      nonNullable: true,
      validators: [Validators.required, Validators.min(10), Validators.max(3600)],
    }),
    idleTimeoutSecs: new FormControl(60, {
      nonNullable: true,
      validators: [Validators.required, Validators.min(5), Validators.max(600)],
    }),
    allowCloudProviders: new FormControl(false, { nonNullable: true }),
    shareMemoryWithCloud: new FormControl(false, { nonNullable: true }),
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

  protected async save(): Promise<void> {
    const current = this.store.settings();
    if (!current || this.form.invalid) {
      this.form.markAllAsTouched();
      return;
    }
    const value = this.form.getRawValue();
    const next: Settings = {
      ...current,
      general: { ...current.general, assistantLanguage: value.assistantLanguage },
      interface: { ...current.interface, reducedMotion: value.reducedMotion },
      ai: {
        ...current.ai,
        requestTimeoutSecs: value.requestTimeoutSecs,
        idleTimeoutSecs: value.idleTimeoutSecs,
      },
      privacy: {
        ...current.privacy,
        allowCloudProviders: value.allowCloudProviders,
        shareMemoryWithCloud: value.shareMemoryWithCloud,
      },
      logging: { ...current.logging, level: value.logLevel },
    };

    if (await this.store.save(next)) {
      this.form.markAsPristine();
      this.savedAt.set(Date.now());
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
    requestTimeoutSecs: ai.requestTimeoutSecs,
    idleTimeoutSecs: ai.idleTimeoutSecs,
    allowCloudProviders: settings.privacy.allowCloudProviders,
    shareMemoryWithCloud: settings.privacy.shareMemoryWithCloud,
    logLevel: settings.logging.level,
  };
}

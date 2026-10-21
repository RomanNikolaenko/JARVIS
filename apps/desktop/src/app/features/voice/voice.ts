import { ChangeDetectionStrategy, Component, DestroyRef, computed, effect, inject, signal } from '@angular/core';
import { toSignal } from '@angular/core/rxjs-interop';
import { FormControl, FormGroup, ReactiveFormsModule, Validators } from '@angular/forms';
import { RouterLink } from '@angular/router';
import type {
  DownloadStatus,
  Settings,
  VoiceLanguage,
  VoicePackage,
  VoicePackageKind,
  VoiceSettings,
} from '@jarvis/shared-types';

import { pollWhileVisible } from '../../core/state/poll';
import { SettingsStore } from '../../core/state/settings.store';
import { VoiceStore } from '../../core/state/voice.store';
import { downloadPercent, formatBytes } from '../../shared/format';
import { HudPanel } from '../../shared/hud-panel/hud-panel';
import { Icon } from '../../shared/icon/icon';

const DOWNLOAD_POLL_MS = 1_000;

const GROUPS: readonly { readonly title: string; readonly kinds: readonly VoicePackageKind[]; readonly hint: string }[] = [
  {
    title: 'Engines',
    kinds: ['speechEngine', 'voiceEngine'],
    hint: 'Official builds that run on this computer: whisper.cpp recognizes speech, Piper speaks.',
  },
  {
    title: 'Speech recognition models',
    kinds: ['speechModel'],
    hint: 'One is enough. Small is the best fit for Ukrainian on an ordinary laptop.',
  },
  { title: 'Voices', kinds: ['voice'], hint: 'Install a Ukrainian and an English voice to hear replies in both languages.' },
];

const SAMPLES: Record<'uk' | 'en', string> = {
  uk: 'Добрий день! Я Джарвіс, ваш помічник. Чим можу допомогти?',
  en: 'Good day! I am JARVIS, your assistant. How can I help?',
};

/** Level meter width in percent for an RMS level (speech is about 0.02–0.2). */
export function levelPercent(level: number): number {
  return Math.min(100, Math.round(Math.sqrt(Math.max(0, level)) * 220));
}

@Component({
  selector: 'jv-voice',
  imports: [ReactiveFormsModule, RouterLink, HudPanel, Icon],
  templateUrl: './voice.html',
  styleUrl: './voice.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class VoicePage {
  protected readonly voice = inject(VoiceStore);
  private readonly settingsStore = inject(SettingsStore);

  protected readonly formatBytes = formatBytes;
  protected readonly downloadPercent = downloadPercent;
  protected readonly levelPercent = levelPercent;
  protected readonly saving = this.settingsStore.saving;
  protected readonly saveError = this.settingsStore.error;
  protected readonly savedAt = signal<number | null>(null);
  protected readonly confirmingInstall = signal<string | null>(null);
  protected readonly confirmingRemoval = signal<string | null>(null);

  protected readonly form = new FormGroup({
    inputDevice: new FormControl('', { nonNullable: true }),
    outputDevice: new FormControl('', { nonNullable: true }),
    language: new FormControl<VoiceLanguage>('auto', { nonNullable: true }),
    sttModel: new FormControl('', { nonNullable: true }),
    voiceUk: new FormControl('', { nonNullable: true }),
    speakerUk: new FormControl(0, { nonNullable: true }),
    voiceEn: new FormControl('', { nonNullable: true }),
    speakerEn: new FormControl(0, { nonNullable: true }),
    speakReplies: new FormControl(true, { nonNullable: true }),
    wakeWord: new FormControl(false, { nonNullable: true }),
    speechRate: new FormControl(1, {
      nonNullable: true,
      validators: [Validators.required, Validators.min(0.5), Validators.max(2)],
    }),
    endSilenceMs: new FormControl(800, {
      nonNullable: true,
      validators: [Validators.required, Validators.min(300), Validators.max(3000)],
    }),
  });

  private readonly formValue = toSignal(this.form.valueChanges, { initialValue: this.form.getRawValue() });

  protected readonly status = this.voice.status;
  protected readonly state = this.voice.state;
  protected readonly loaded = computed(() => this.settingsStore.settings() !== null && this.status() !== null);
  protected readonly groups = computed(() => {
    const packages = this.status()?.packages ?? [];
    return GROUPS.map((group) => ({ ...group, packages: packages.filter((p) => group.kinds.includes(p.kind)) }));
  });
  protected readonly installedModels = computed(() => this.installed('speechModel'));
  protected readonly ukVoices = computed(() => this.installed('voice', 'uk'));
  protected readonly enVoices = computed(() => this.installed('voice', 'en'));
  protected readonly speakersUk = computed(() => this.speakersOf(this.formValue().voiceUk, 'uk'));
  protected readonly speakersEn = computed(() => this.speakersOf(this.formValue().voiceEn, 'en'));
  protected readonly testing = signal(false);

  constructor() {
    const destroyRef = inject(DestroyRef);
    void this.voice.refresh();
    // Progress of downloads.
    pollWhileVisible(
      () => {
        if (this.voice.downloading()) {
          void this.voice.refresh();
        }
      },
      DOWNLOAD_POLL_MS,
      destroyRef
    );
    effect(() => {
      const settings = this.settingsStore.settings();
      if (settings && !this.form.dirty) {
        this.form.reset(toFormValue(settings.voice));
      }
    });
    // The test ends when the microphone closes.
    effect(() => {
      if (!this.voice.micOpen() && this.voice.phase() === 'idle') {
        this.testing.set(false);
      }
    });
  }

  protected downloadFor(pkg: VoicePackage): DownloadStatus | undefined {
    return this.voice
      .downloads()
      .find((d) => d.label === pkg.displayName && !['completed', 'failed', 'cancelled'].includes(d.phase));
  }

  protected failedDownload(pkg: VoicePackage): DownloadStatus | undefined {
    const last = this.voice
      .downloads()
      .filter((d) => d.label === pkg.displayName)
      .at(-1);
    return last?.phase === 'failed' ? last : undefined;
  }

  protected async install(pkg: VoicePackage): Promise<void> {
    this.confirmingInstall.set(null);
    await this.voice.install(pkg);
  }

  protected async remove(pkg: VoicePackage): Promise<void> {
    this.confirmingRemoval.set(null);
    await this.voice.remove(pkg);
  }

  protected async testMicrophone(): Promise<void> {
    if (this.voice.micOpen()) {
      await this.voice.submit();
      return;
    }
    this.testing.set(true);
    await this.voice.listen(true);
  }

  protected testVoice(language: 'uk' | 'en'): Promise<void> {
    return this.voice.say(SAMPLES[language]);
  }

  protected async save(): Promise<void> {
    const current = this.settingsStore.settings();
    if (!current || this.form.invalid) {
      this.form.markAllAsTouched();
      return;
    }
    const next: Settings = { ...current, voice: fromFormValue(this.form.getRawValue()) };
    if (await this.settingsStore.save(next)) {
      this.form.markAsPristine();
      this.savedAt.set(Date.now());
      await this.voice.refresh();
    }
  }

  protected discard(): void {
    const settings = this.settingsStore.settings();
    if (settings) {
      this.form.reset(toFormValue(settings.voice));
    }
  }

  private installed(kind: VoicePackageKind, language?: string): VoicePackage[] {
    return (this.status()?.packages ?? []).filter(
      (p) => p.kind === kind && p.installed && (language === undefined || p.language === language)
    );
  }

  /** Speakers of the chosen voice (or of the one used when none is chosen). */
  private speakersOf(id: string | undefined, language: 'uk' | 'en'): readonly string[] {
    const voices = language === 'uk' ? this.ukVoices() : this.enVoices();
    const voice = voices.find((v) => v.id === id) ?? voices.find((v) => v.recommended) ?? voices[0];
    return voice?.speakers ?? [];
  }
}

function toFormValue(voice: VoiceSettings) {
  return {
    inputDevice: voice.inputDevice ?? '',
    outputDevice: voice.outputDevice ?? '',
    language: voice.language,
    sttModel: voice.sttModel ?? '',
    voiceUk: voice.voiceUk ?? '',
    speakerUk: voice.speakerUk,
    voiceEn: voice.voiceEn ?? '',
    speakerEn: voice.speakerEn,
    speakReplies: voice.speakReplies,
    wakeWord: voice.wakeWord,
    speechRate: voice.speechRate,
    endSilenceMs: voice.endSilenceMs,
  };
}

function fromFormValue(value: ReturnType<typeof toFormValue>): VoiceSettings {
  const optional = (text: string): string | null => (text === '' ? null : text);
  return {
    inputDevice: optional(value.inputDevice),
    outputDevice: optional(value.outputDevice),
    language: value.language,
    sttModel: optional(value.sttModel),
    voiceUk: optional(value.voiceUk),
    speakerUk: Number(value.speakerUk),
    voiceEn: optional(value.voiceEn),
    speakerEn: Number(value.speakerEn),
    speakReplies: value.speakReplies,
    wakeWord: value.wakeWord,
    speechRate: Number(value.speechRate),
    endSilenceMs: Number(value.endSilenceMs),
  };
}

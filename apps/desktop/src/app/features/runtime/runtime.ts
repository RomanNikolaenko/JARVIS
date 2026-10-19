import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  computed,
  effect,
  inject,
  signal,
} from '@angular/core';
import { FormControl, FormGroup, ReactiveFormsModule, Validators } from '@angular/forms';
import type {
  BinarySource,
  DownloadStatus,
  GpuOffload,
  LocalModel,
  RuntimeBackend,
  RuntimeSettings,
} from '@jarvis/shared-types';

import { pollWhileVisible } from '../../core/state/poll';
import { RuntimeStore, recommendedBackend } from '../../core/state/runtime.store';
import { formatBytes, formatUptime } from '../../shared/format';
import { HudPanel } from '../../shared/hud-panel/hud-panel';
import { Icon } from '../../shared/icon/icon';
import { StatusBadge } from '../../shared/status-badge/status-badge';

const POLL_MS = 1_500;
/** llama.cpp treats any value above the layer count as "all layers". */
const ALL_LAYERS = 999;

export const BACKEND_LABELS: Record<RuntimeBackend, string> = {
  cpu: 'CPU',
  vulkan: 'Vulkan (GPU)',
  cuda: 'CUDA (NVIDIA)',
  other: 'Other GPU backend',
};

export function describeSource(source: BinarySource): string {
  return source.kind === 'release'
    ? `Official llama.cpp ${source.tag} (${source.asset})`
    : 'Selected by you';
}

export function downloadPercent(download: DownloadStatus): number {
  return download.totalBytes
    ? Math.min(100, Math.round((download.receivedBytes / download.totalBytes) * 100))
    : 0;
}

@Component({
  selector: 'jv-runtime',
  imports: [ReactiveFormsModule, HudPanel, Icon, StatusBadge],
  templateUrl: './runtime.html',
  styleUrl: './runtime.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class RuntimePage {
  protected readonly store = inject(RuntimeStore);

  protected readonly formatBytes = formatBytes;
  protected readonly backendLabels = BACKEND_LABELS;
  protected readonly describeSource = describeSource;
  protected readonly downloadPercent = downloadPercent;
  protected readonly allLayers = ALL_LAYERS;

  protected readonly backend = signal<RuntimeBackend>('cpu');
  protected readonly confirmingRemoval = signal<string | null>(null);
  protected readonly now = signal(Date.now());

  protected readonly recommended = computed(() => recommendedBackend(this.store.hardware()));
  protected readonly models = computed(() => this.store.status()?.models ?? []);
  protected readonly runtimeDownloads = computed(() =>
    (this.store.status()?.downloads ?? []).filter((d) => d.kind === 'runtime'),
  );
  protected readonly modelDownloads = computed(() =>
    (this.store.status()?.downloads ?? []).filter((d) => d.kind === 'model'),
  );
  protected readonly serverActive = computed(() => {
    const phase = this.store.server()?.phase;
    return phase === 'starting' || phase === 'running';
  });
  protected readonly uptime = computed(() => {
    const started = this.store.server()?.startedAtMs;
    return started ? formatUptime(Math.max(0, (this.now() - started) / 1000)) : '—';
  });

  protected readonly customDownload = new FormGroup({
    repo: new FormControl('', {
      nonNullable: true,
      validators: [Validators.required, Validators.pattern(/^[\w.-]+\/[\w.-]+$/)],
    }),
    file: new FormControl('', {
      nonNullable: true,
      validators: [Validators.required, Validators.pattern(/\.gguf$/i)],
    }),
    revision: new FormControl('', { nonNullable: true }),
  });

  protected readonly settingsForm = new FormGroup({
    contextSize: new FormControl(4096, {
      nonNullable: true,
      validators: [Validators.required, Validators.min(512), Validators.max(262_144)],
    }),
    threads: new FormControl<number | null>(null, {
      validators: [Validators.min(1), Validators.max(256)],
    }),
    gpuOffload: new FormControl<GpuOffload>('auto', { nonNullable: true }),
    gpuLayers: new FormControl(0, {
      nonNullable: true,
      validators: [Validators.min(0), Validators.max(1000)],
    }),
    autostartModel: new FormControl('', { nonNullable: true }),
    startupTimeoutSecs: new FormControl(180, {
      nonNullable: true,
      validators: [Validators.required, Validators.min(10), Validators.max(1800)],
    }),
  });
  protected readonly settingsSaved = signal(false);

  constructor() {
    const destroyRef = inject(DestroyRef);
    void this.store.loadStatic();
    pollWhileVisible(
      () => {
        this.now.set(Date.now());
        return this.store.refresh();
      },
      POLL_MS,
      destroyRef,
    );
    // Preselect the recommended backend once hardware is known.
    effect(() => this.backend.set(this.recommended()));
    // Mirror stored settings into the form unless the user is editing.
    effect(() => {
      const settings = this.store.settings();
      if (settings && !this.settingsForm.dirty) {
        this.settingsForm.reset({
          ...settings,
          autostartModel: settings.autostartModel ?? '',
        });
      }
    });
  }

  protected onBackend(event: Event): void {
    this.backend.set((event.target as HTMLSelectElement).value as RuntimeBackend);
    this.store.release.set(null);
  }

  protected canStart(model: LocalModel): boolean {
    return (
      !!this.store.installed() &&
      model.available &&
      !(this.serverActive() && this.store.server()?.modelId === model.id)
    );
  }

  protected downloadCustom(): void {
    if (this.customDownload.invalid) {
      this.customDownload.markAllAsTouched();
      return;
    }
    const { repo, file, revision } = this.customDownload.getRawValue();
    void this.store
      .downloadModel({ repo: repo.trim(), file: file.trim(), revision: revision.trim() || null })
      .then(() => {
        if (!this.store.error()) {
          this.customDownload.reset();
        }
      });
  }

  protected isCuratedPresent(id: string): boolean {
    return this.models().some((m) => m.id === id && m.available);
  }

  protected async remove(model: LocalModel, deleteFile: boolean): Promise<void> {
    this.confirmingRemoval.set(null);
    await this.store.removeModel(model.id, deleteFile);
  }

  protected async saveSettings(): Promise<void> {
    if (this.settingsForm.invalid) {
      this.settingsForm.markAllAsTouched();
      return;
    }
    const value = this.settingsForm.getRawValue();
    const runtime: RuntimeSettings = {
      contextSize: value.contextSize,
      threads: value.threads || null,
      gpuOffload: value.gpuOffload,
      gpuLayers: value.gpuLayers,
      autostartModel: value.autostartModel || null,
      startupTimeoutSecs: value.startupTimeoutSecs,
    };
    if (await this.store.saveSettings(runtime)) {
      this.settingsForm.markAsPristine();
      this.settingsSaved.set(true);
    }
  }
}

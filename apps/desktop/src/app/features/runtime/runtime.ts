import { ChangeDetectionStrategy, Component, DestroyRef, computed, effect, inject, signal } from '@angular/core';
import { FormControl, FormGroup, ReactiveFormsModule, Validators } from '@angular/forms';
import type {
  BinarySource,
  DownloadStatus,
  GpuOffload,
  LocalModel,
  RuntimeBackend,
  RuntimeSettings,
  ServerPhase,
  ServerRole,
  ServerState,
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
  return source.kind === 'release' ? `Official llama.cpp ${source.tag} (${source.asset})` : 'Selected by you';
}

export const ROLE_LABELS: Record<ServerRole, string> = {
  chat: 'Chat model',
  embeddings: 'Embedding model',
};

/**
 * Embedding models declare how they pool token vectors (sentence-transformers
 * conversions); chat models and rerankers do not produce search vectors.
 */
export function isEmbeddingModel(model: LocalModel): boolean {
  const pooling = model.gguf.pooling;
  return pooling === 'mean' || pooling === 'cls' || pooling === 'last';
}

/** The server a library model is started in. */
export function roleOf(model: LocalModel): ServerRole {
  return isEmbeddingModel(model) ? 'embeddings' : 'chat';
}

/** Health-style status of a server phase (for the status badge). */
export function phaseBadge(phase: ServerPhase): 'healthy' | 'unhealthy' | 'unknown' | 'degraded' {
  switch (phase) {
    case 'running':
      return 'healthy';
    case 'failed':
      return 'unhealthy';
    case 'stopped':
      return 'unknown';
    default:
      return 'degraded';
  }
}

export function isActive(server: ServerState | null | undefined): boolean {
  return server?.phase === 'starting' || server?.phase === 'running';
}

/** One server as shown on the screen. */
interface ServerView {
  role: ServerRole;
  state: ServerState;
  logTail: readonly string[];
}

export function downloadPercent(download: DownloadStatus): number {
  return download.totalBytes ? Math.min(100, Math.round((download.receivedBytes / download.totalBytes) * 100)) : 0;
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
  protected readonly roleLabels = ROLE_LABELS;
  protected readonly roleOf = roleOf;
  protected readonly isEmbeddingModel = isEmbeddingModel;
  protected readonly phaseBadge = phaseBadge;
  protected readonly isActive = isActive;

  protected readonly backend = signal<RuntimeBackend>('cpu');
  protected readonly confirmingRemoval = signal<string | null>(null);
  protected readonly now = signal(Date.now());

  protected readonly recommended = computed(() => recommendedBackend(this.store.hardware()));
  protected readonly models = computed(() => this.store.status()?.models ?? []);
  protected readonly chatModels = computed(() => this.models().filter((m) => !isEmbeddingModel(m)));
  protected readonly embeddingModels = computed(() => this.models().filter(isEmbeddingModel));
  protected readonly servers = computed<ServerView[]>(() => {
    const status = this.store.status();
    if (!status) {
      return [];
    }
    return [
      { role: 'chat', state: status.server, logTail: status.logTail },
      { role: 'embeddings', state: status.embeddingServer, logTail: status.embeddingLogTail },
    ];
  });
  protected readonly runtimeDownloads = computed(() =>
    (this.store.status()?.downloads ?? []).filter((d) => d.kind === 'runtime')
  );
  protected readonly modelDownloads = computed(() => (this.store.status()?.downloads ?? []).filter((d) => d.kind === 'model'));

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
    autostartEmbeddingModel: new FormControl('', { nonNullable: true }),
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
      destroyRef
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
          autostartEmbeddingModel: settings.autostartEmbeddingModel ?? '',
        });
      }
    });
  }

  protected onBackend(event: Event): void {
    this.backend.set((event.target as HTMLSelectElement).value as RuntimeBackend);
    this.store.release.set(null);
  }

  protected uptime(server: ServerState): string {
    const started = server.startedAtMs;
    return started ? formatUptime(Math.max(0, (this.now() - started) / 1000)) : '—';
  }

  protected canStart(model: LocalModel): boolean {
    const server = roleOf(model) === 'embeddings' ? this.store.embeddingServer() : this.store.server();
    return !!this.store.installed() && model.available && !(isActive(server) && server?.modelId === model.id);
  }

  protected downloadCustom(): void {
    if (this.customDownload.invalid) {
      this.customDownload.markAllAsTouched();
      return;
    }
    const { repo, file, revision } = this.customDownload.getRawValue();
    void this.store.downloadModel({ repo: repo.trim(), file: file.trim(), revision: revision.trim() || null }).then(() => {
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
      autostartEmbeddingModel: value.autostartEmbeddingModel || null,
      startupTimeoutSecs: value.startupTimeoutSecs,
    };
    if (await this.store.saveSettings(runtime)) {
      this.settingsForm.markAsPristine();
      this.settingsSaved.set(true);
    }
  }
}

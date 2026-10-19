import { Injectable, computed, inject, signal } from '@angular/core';
import type {
  CuratedModel,
  HardwareInfo,
  ModelDownloadRequest,
  RuntimeBackend,
  RuntimeRelease,
  RuntimeSettings,
  RuntimeStatus,
  ServerPhase,
} from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';
import { ModelsStore } from './models.store';
import { SettingsStore } from './settings.store';

/** Vulkan when a GPU and a Vulkan loader exist, otherwise CPU. */
export function recommendedBackend(hardware: HardwareInfo | null): RuntimeBackend {
  return hardware && hardware.vulkanAvailable && hardware.gpus.length > 0 ? 'vulkan' : 'cpu';
}

/** State of the embedded llama.cpp runtime (Runtime screen). */
@Injectable({ providedIn: 'root' })
export class RuntimeStore {
  private readonly api = inject(JarvisApi);
  private readonly models = inject(ModelsStore);
  private readonly settingsStore = inject(SettingsStore);
  private lastPhase: ServerPhase | null = null;
  private refreshing = false;

  readonly status = signal<RuntimeStatus | null>(null);
  readonly hardware = signal<HardwareInfo | null>(null);
  readonly curated = signal<readonly CuratedModel[]>([]);
  readonly release = signal<RuntimeRelease | null>(null);
  readonly checkingRelease = signal(false);
  /** Last failed action, shown on the screen. */
  readonly error = signal<IpcError | null>(null);

  readonly server = computed(() => this.status()?.server ?? null);
  readonly installed = computed(() => this.status()?.binary ?? null);
  readonly busy = computed(() => {
    const status = this.status();
    if (!status) {
      return false;
    }
    return (
      status.server.phase === 'starting' ||
      status.server.phase === 'stopping' ||
      status.downloads.some((d) => !['completed', 'failed', 'cancelled'].includes(d.phase))
    );
  });
  readonly settings = computed<RuntimeSettings | null>(
    () => this.settingsStore.settings()?.runtime ?? null,
  );

  async refresh(): Promise<void> {
    if (this.refreshing) {
      return;
    }
    this.refreshing = true;
    try {
      const status = await this.api.getRuntimeStatus();
      this.status.set(status);
      const phase = status.server.phase;
      // The set of AI models changes when a local model starts or stops.
      if (
        this.lastPhase !== null &&
        phase !== this.lastPhase &&
        (phase === 'running' || this.lastPhase === 'running')
      ) {
        void this.models.refresh(true);
      }
      this.lastPhase = phase;
    } catch (error) {
      this.error.set(IpcError.from(error));
    } finally {
      this.refreshing = false;
    }
  }

  async loadStatic(): Promise<void> {
    await this.run(async () => {
      const [hardware, curated] = await Promise.all([
        this.api.getHardwareInfo(),
        this.api.listCuratedModels(),
      ]);
      this.hardware.set(hardware);
      this.curated.set(curated);
    });
  }

  async checkRelease(backend: RuntimeBackend): Promise<void> {
    this.checkingRelease.set(true);
    this.release.set(null);
    await this.run(async () => this.release.set(await this.api.checkRuntimeRelease(backend)));
    this.checkingRelease.set(false);
  }

  async install(release: RuntimeRelease): Promise<void> {
    await this.run(async () => {
      await this.api.installRuntime(release);
      this.release.set(null);
    });
  }

  selectBinary(): Promise<void> {
    return this.run(() => this.api.selectRuntimeBinary());
  }

  importModel(): Promise<void> {
    return this.run(() => this.api.importModel());
  }

  downloadCurated(id: string): Promise<void> {
    return this.run(() => this.api.downloadCuratedModel(id));
  }

  downloadModel(request: ModelDownloadRequest): Promise<void> {
    return this.run(() => this.api.downloadModel(request));
  }

  cancelDownload(id: string): Promise<void> {
    return this.run(() => this.api.cancelDownload(id));
  }

  removeModel(id: string, deleteFile: boolean): Promise<void> {
    return this.run(() => this.api.removeModel(id, deleteFile));
  }

  start(modelId: string): Promise<void> {
    return this.run(() => this.api.startLocalModel(modelId));
  }

  stop(): Promise<void> {
    return this.run(() => this.api.stopLocalModel());
  }

  async saveSettings(runtime: RuntimeSettings): Promise<boolean> {
    const settings = this.settingsStore.settings();
    return settings ? this.settingsStore.save({ ...settings, runtime }) : false;
  }

  /** Runs an action, records its error and refreshes the status. */
  private async run(action: () => Promise<unknown>): Promise<void> {
    this.error.set(null);
    try {
      await action();
    } catch (error) {
      this.error.set(IpcError.from(error));
    }
    await this.refresh();
  }
}

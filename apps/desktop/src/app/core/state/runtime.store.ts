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
  ServerRole,
  ServerState,
} from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';
import { ModelsStore } from './models.store';
import { SettingsStore } from './settings.store';

/** Dedicated GPU memory from which the Vulkan build is recommended. */
export const VULKAN_MIN_VRAM_BYTES = 4 * 1024 ** 3;

/**
 * Vulkan when a Vulkan loader and a GPU with enough dedicated memory exist,
 * otherwise CPU. Integrated GPUs share system memory, so they rarely beat the
 * CPU build, and some older drivers compute wrong results (seen with Radeon
 * Vega 8); users can still pick Vulkan.
 */
export function recommendedBackend(hardware: HardwareInfo | null): RuntimeBackend {
  const capable = hardware?.gpus.some((gpu) => (gpu.vramTotalBytes ?? 0) >= VULKAN_MIN_VRAM_BYTES);
  return hardware?.vulkanAvailable && capable ? 'vulkan' : 'cpu';
}

function settling(server: ServerState): boolean {
  return server.phase === 'starting' || server.phase === 'stopping';
}

/** State of the embedded llama.cpp runtime (Runtime screen). */
@Injectable({ providedIn: 'root' })
export class RuntimeStore {
  private readonly api = inject(JarvisApi);
  private readonly models = inject(ModelsStore);
  private readonly settingsStore = inject(SettingsStore);
  /** Last seen phase of the chat and the embedding server. */
  private lastPhases: [ServerPhase, ServerPhase] | null = null;
  private refreshing = false;

  readonly status = signal<RuntimeStatus | null>(null);
  readonly hardware = signal<HardwareInfo | null>(null);
  readonly curated = signal<readonly CuratedModel[]>([]);
  readonly release = signal<RuntimeRelease | null>(null);
  readonly checkingRelease = signal(false);
  /** Last failed action, shown on the screen. */
  readonly error = signal<IpcError | null>(null);

  /** The chat model server. */
  readonly server = computed(() => this.status()?.server ?? null);
  /** The embedding model server (semantic memory search). */
  readonly embeddingServer = computed(() => this.status()?.embeddingServer ?? null);
  readonly installed = computed(() => this.status()?.binary ?? null);
  readonly busy = computed(() => {
    const status = this.status();
    if (!status) {
      return false;
    }
    return (
      settling(status.server) ||
      settling(status.embeddingServer) ||
      status.downloads.some((d) => !['completed', 'failed', 'cancelled'].includes(d.phase))
    );
  });
  readonly settings = computed<RuntimeSettings | null>(() => this.settingsStore.settings()?.runtime ?? null);

  async refresh(): Promise<void> {
    if (this.refreshing) {
      return;
    }
    this.refreshing = true;
    try {
      const status = await this.api.getRuntimeStatus();
      this.status.set(status);
      const phases: [ServerPhase, ServerPhase] = [status.server.phase, status.embeddingServer.phase];
      // The set of AI models changes when a local model starts or stops.
      const last = this.lastPhases;
      const changed = phases.some(
        (phase, i) => last !== null && phase !== last[i] && (phase === 'running' || last[i] === 'running')
      );
      if (changed) {
        void this.models.refresh(true);
      }
      this.lastPhases = phases;
    } catch (error) {
      this.error.set(IpcError.from(error));
    } finally {
      this.refreshing = false;
    }
  }

  async loadStatic(): Promise<void> {
    await this.run(async () => {
      const [hardware, curated] = await Promise.all([this.api.getHardwareInfo(), this.api.listCuratedModels()]);
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

  /** Lets the user pick a vision projector (mmproj) for a model. */
  attachProjector(modelId: string): Promise<void> {
    return this.run(() => this.api.attachProjector(modelId));
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

  start(modelId: string, role: ServerRole): Promise<void> {
    return this.run(() => this.api.startLocalModel(modelId, role));
  }

  stop(role: ServerRole): Promise<void> {
    return this.run(() => this.api.stopLocalModel(role));
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

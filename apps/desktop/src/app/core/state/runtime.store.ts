import { DestroyRef, Injectable, computed, inject, signal } from '@angular/core';
import type {
  CuratedModel,
  HardwareInfo,
  ModelDownloadRequest,
  RuntimeBackend,
  RuntimeRelease,
  RuntimeSettings,
  RuntimeStatus,
  ServerRole,
  ServerState,
} from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';
import { withRequestDeadline } from '../ipc/request-deadline';
import { ModelsStore } from './models.store';
import { SettingsStore } from './settings.store';

/** Dedicated GPU memory from which the Vulkan build is recommended. */
export const VULKAN_MIN_VRAM_BYTES = 4 * 1024 ** 3;
export const RUNTIME_STATUS_TIMEOUT_MS = 10_000;
export const RUNTIME_RELEASE_TIMEOUT_MS = 30_000;

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

/** Only changes that affect the available model or its advertised capabilities. */
function servedModelKey(server: ServerState): string | null {
  return server.phase === 'running'
    ? JSON.stringify([server.modelId, server.port, server.contextSize, server.vision, server.toolCalling])
    : null;
}

/** State of the embedded llama.cpp runtime (Runtime screen). */
@Injectable({ providedIn: 'root' })
export class RuntimeStore {
  private readonly api = inject(JarvisApi);
  private readonly destroyRef = inject(DestroyRef);
  private readonly models = inject(ModelsStore);
  private readonly settingsStore = inject(SettingsStore);
  private lastServedModels: [string | null, string | null] = [null, null];
  private refreshing: Promise<void> | null = null;
  private releaseEpoch = 0;
  private releaseRequest: { backend: RuntimeBackend; promise: Promise<void> } | null = null;
  private readonly commands = new Map<string, Promise<boolean>>();
  private readonly pendingCommands = signal<ReadonlySet<string>>(new Set());
  readonly commandPending = computed(() => this.pendingCommands().size > 0);

  isPending(key: string): boolean {
    return this.pendingCommands().has(key);
  }

  readonly status = signal<RuntimeStatus | null>(null);
  readonly hardware = signal<HardwareInfo | null>(null);
  readonly curated = signal<readonly CuratedModel[]>([]);
  readonly release = signal<RuntimeRelease | null>(null);
  readonly checkingRelease = signal(false);
  /** Failures remain visible until the same resource is retried or dismissed. */
  readonly actionErrors = signal<readonly { key: string; label: string; error: IpcError }[]>([]);
  readonly statusError = signal<IpcError | null>(null);

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
  readonly savingSettings = this.settingsStore.saving;
  readonly settingsError = this.settingsStore.error;

  refresh(): Promise<void> {
    if (this.destroyRef.destroyed) return Promise.resolve();
    if (this.refreshing) return this.refreshing;
    this.refreshing = this.readStatus().finally(() => {
      this.refreshing = null;
    });
    return this.refreshing;
  }

  private async readStatus(): Promise<void> {
    try {
      const status = await withRequestDeadline(
        () => this.api.getRuntimeStatus(),
        RUNTIME_STATUS_TIMEOUT_MS,
        'Runtime status',
        this.destroyRef
      );
      if (this.destroyRef.destroyed) return;
      this.status.set(status);
      this.statusError.set(null);
      const servedModels: [string | null, string | null] = [
        servedModelKey(status.server),
        servedModelKey(status.embeddingServer),
      ];
      // Polling can miss intermediate phases when a model is replaced or restarted.
      const changed = servedModels.some((model, i) => model !== this.lastServedModels[i]);
      this.lastServedModels = servedModels;
      if (changed) {
        void this.models.refresh(true);
      }
    } catch (error) {
      if (!this.destroyRef.destroyed) this.statusError.set(IpcError.from(error));
    }
  }

  async loadStatic(): Promise<void> {
    await this.command('catalog', 'Load model catalog', async () => {
      const [hardware, curated] = await Promise.all([this.api.getHardwareInfo(), this.api.listCuratedModels()]);
      this.hardware.set(hardware);
      this.curated.set(curated);
    });
  }

  clearRelease(): void {
    this.releaseEpoch++;
    this.releaseRequest = null;
    this.release.set(null);
    this.checkingRelease.set(false);
    this.dismissError('release');
  }

  checkRelease(backend: RuntimeBackend): Promise<void> {
    if (this.destroyRef.destroyed) return Promise.resolve();
    if (this.releaseRequest?.backend === backend) return this.releaseRequest.promise;
    this.clearRelease();
    const epoch = this.releaseEpoch;
    this.checkingRelease.set(true);
    const promise = this.readRelease(backend, epoch);
    this.releaseRequest = { backend, promise };
    return promise;
  }

  private async readRelease(backend: RuntimeBackend, epoch: number): Promise<void> {
    try {
      const release = await withRequestDeadline(
        () => this.api.checkRuntimeRelease(backend),
        RUNTIME_RELEASE_TIMEOUT_MS,
        'Runtime release',
        this.destroyRef
      );
      if (epoch === this.releaseEpoch && !this.destroyRef.destroyed) this.release.set(release);
    } catch (error) {
      if (epoch === this.releaseEpoch && !this.destroyRef.destroyed) {
        this.actionErrors.update((errors) => [
          ...errors,
          { key: 'release', label: 'Check runtime release', error: IpcError.from(error) },
        ]);
      }
    } finally {
      if (epoch === this.releaseEpoch) {
        this.releaseRequest = null;
        this.checkingRelease.set(false);
      }
    }
  }

  async install(release: RuntimeRelease): Promise<void> {
    await this.command('engine', 'Install runtime', async () => {
      await this.api.installRuntime(release);
      if (this.release() === release) this.release.set(null);
    });
  }

  selectBinary(): Promise<boolean> {
    return this.command('engine', 'Select runtime binary', () => this.api.selectRuntimeBinary());
  }

  importModel(): Promise<boolean> {
    return this.command('import', 'Import model', () => this.api.importModel());
  }

  /** Lets the user pick a vision projector (mmproj) for a model. */
  attachProjector(modelId: string): Promise<boolean> {
    return this.command(`model:${modelId}`, `Add vision to ${modelId}`, () => this.api.attachProjector(modelId));
  }

  downloadCurated(id: string): Promise<boolean> {
    if (this.isCuratedDownloading(id)) return Promise.resolve(false);
    return this.command(`curated:${id}`, `Download ${id}`, () => this.api.downloadCuratedModel(id));
  }

  isCuratedDownloading(id: string): boolean {
    const model = this.curated().find((entry) => entry.id === id);
    if (!model) return false;
    const labels = [model.displayName, `${model.repo} / ${model.file}`];
    return (
      this.status()?.downloads.some(
        (download) =>
          download.kind === 'model' &&
          labels.includes(download.label) &&
          !['completed', 'failed', 'cancelled'].includes(download.phase)
      ) ?? false
    );
  }

  downloadModel(request: ModelDownloadRequest): Promise<boolean> {
    return this.command('custom-download', `Download ${request.file}`, () => this.api.downloadModel(request));
  }

  cancelDownload(id: string): Promise<boolean> {
    return this.command(`download:${id}`, 'Cancel download', () => this.api.cancelDownload(id));
  }

  removeModel(id: string, deleteFile: boolean): Promise<boolean> {
    return this.command(`model:${id}`, `Remove ${id}`, () => this.api.removeModel(id, deleteFile));
  }

  start(modelId: string, role: ServerRole): Promise<boolean> {
    return this.command(`server:${role}`, `Start ${role} model`, () => this.api.startLocalModel(modelId, role));
  }

  stop(role: ServerRole): Promise<boolean> {
    return this.command(`server:${role}`, `Stop ${role} model`, () => this.api.stopLocalModel(role));
  }

  async saveSettings(runtime: RuntimeSettings): Promise<boolean> {
    const settings = this.settingsStore.settings();
    return settings ? this.settingsStore.save({ ...settings, runtime }) : false;
  }

  /** One mutation per resource until its result and following status read settle. */
  private command(key: string, label: string, action: () => Promise<unknown>): Promise<boolean> {
    if (this.destroyRef.destroyed) return Promise.resolve(false);
    const current = this.commands.get(key);
    if (current) return current;
    this.pendingCommands.update((keys) => new Set([...keys, key]));
    const pending = this.run(key, label, action).finally(() => {
      this.commands.delete(key);
      this.pendingCommands.update((keys) => {
        const next = new Set(keys);
        next.delete(key);
        return next;
      });
    });
    this.commands.set(key, pending);
    return pending;
  }

  dismissError(key: string): void {
    this.actionErrors.update((errors) => errors.filter((entry) => entry.key !== key));
  }

  /** Returns this action's outcome independently of other actions and status reads. */
  private async run(key: string, label: string, action: () => Promise<unknown>): Promise<boolean> {
    this.dismissError(key);
    let succeeded = true;
    try {
      await action();
    } catch (error) {
      succeeded = false;
      if (!this.destroyRef.destroyed) {
        this.actionErrors.update((errors) => [...errors, { key, label, error: IpcError.from(error) }]);
      }
    }
    // A poll started before the action may return its old state. Wait for it,
    // then request a snapshot taken after the action completed.
    if (this.refreshing) await this.refreshing;
    await this.refresh();
    return succeeded && !this.destroyRef.destroyed;
  }
}

import { TestBed } from '@angular/core/testing';
import type { HardwareInfo, RuntimeStatus, ServerPhase, ServerState } from '@jarvis/shared-types';

import { JarvisApi } from '../ipc/jarvis-api';
import { ModelsStore } from './models.store';
import { RuntimeStore, recommendedBackend } from './runtime.store';

function server(phase: ServerPhase, model: string): ServerState {
  return {
    phase,
    modelId: phase === 'stopped' ? null : model,
    port: null,
    pid: null,
    startedAtMs: null,
    contextSize: null,
    gpuLayers: null,
    threads: null,
    memoryBytes: null,
    error: null,
  };
}

function status(phase: ServerPhase, embeddingPhase: ServerPhase = 'stopped'): RuntimeStatus {
  return {
    binary: null,
    server: server(phase, 'qwen'),
    embeddingServer: server(embeddingPhase, 'bge-m3-q8_0'),
    models: [],
    downloads: [],
    modelsDir: '/data/models',
    logTail: [],
    embeddingLogTail: [],
  };
}

const HARDWARE: HardwareInfo = {
  cpuBrand: 'CPU',
  physicalCores: 4,
  logicalCpus: 8,
  cpuFeatures: { avx: true, avx2: true, avx512f: false, fma: true, neon: false },
  totalMemoryBytes: 32e9,
  availableMemoryBytes: 20e9,
  gpus: [{ name: 'GPU', vendor: 'amd', vramTotalBytes: 2e9, vramFreeBytes: null }],
  vulkanAvailable: true,
  nvidiaDriver: null,
};

describe('RuntimeStore', () => {
  let api: Record<string, ReturnType<typeof vi.fn>>;
  let refreshModels: ReturnType<typeof vi.fn>;
  let store: RuntimeStore;

  beforeEach(() => {
    api = {
      getRuntimeStatus: vi.fn(async () => status('stopped')),
      startLocalModel: vi.fn(async () => undefined),
      stopLocalModel: vi.fn(async () => undefined),
      removeModel: vi.fn(async () => {
        throw { code: 'permissionDenied', message: 'JARVIS only deletes files it downloaded' };
      }),
    };
    refreshModels = vi.fn(async () => undefined);
    TestBed.configureTestingModule({
      providers: [
        { provide: JarvisApi, useValue: api },
        { provide: ModelsStore, useValue: { refresh: refreshModels } },
      ],
    });
    store = TestBed.inject(RuntimeStore);
  });

  it('recommends Vulkan only with a Vulkan loader and a GPU with 4 GB or more', () => {
    const gpu = HARDWARE.gpus[0];
    const discrete = { ...HARDWARE, gpus: [{ ...gpu, vramTotalBytes: 8 * 1024 ** 3 }] };
    expect(recommendedBackend(discrete)).toBe('vulkan');
    expect(recommendedBackend({ ...discrete, vulkanAvailable: false })).toBe('cpu');
    // A small integrated GPU (Radeon Vega 8: 2 GB) or unknown memory: CPU.
    expect(recommendedBackend(HARDWARE)).toBe('cpu');
    expect(recommendedBackend({ ...HARDWARE, gpus: [{ ...gpu, vramTotalBytes: null }] })).toBe(
      'cpu',
    );
    expect(recommendedBackend({ ...HARDWARE, gpus: [] })).toBe('cpu');
    expect(recommendedBackend(null)).toBe('cpu');
  });

  it('refreshes the AI models when a local model starts or stops', async () => {
    await store.refresh();
    expect(refreshModels).not.toHaveBeenCalled();

    api['getRuntimeStatus']!.mockResolvedValueOnce(status('starting'));
    await store.refresh();
    expect(refreshModels).not.toHaveBeenCalled();

    api['getRuntimeStatus']!.mockResolvedValueOnce(status('running'));
    await store.refresh();
    expect(refreshModels).toHaveBeenCalledTimes(1);

    api['getRuntimeStatus']!.mockResolvedValueOnce(status('stopped'));
    await store.refresh();
    expect(refreshModels).toHaveBeenCalledTimes(2);

    // The embedding server adds and removes an AI provider as well.
    api['getRuntimeStatus']!.mockResolvedValueOnce(status('stopped', 'running'));
    await store.refresh();
    expect(refreshModels).toHaveBeenCalledTimes(3);
    expect(store.embeddingServer()?.modelId).toBe('bge-m3-q8_0');

    api['getRuntimeStatus']!.mockResolvedValueOnce(status('stopped', 'failed'));
    await store.refresh();
    expect(refreshModels).toHaveBeenCalledTimes(4);
  });

  it('records action errors and still refreshes the status', async () => {
    await store.removeModel('m', true);
    expect(store.error()?.code).toBe('permissionDenied');
    expect(api['getRuntimeStatus']).toHaveBeenCalled();

    await store.start('m', 'chat');
    expect(store.error()).toBeNull();
    expect(api['startLocalModel']).toHaveBeenCalledWith('m', 'chat');

    await store.start('bge', 'embeddings');
    expect(api['startLocalModel']).toHaveBeenCalledWith('bge', 'embeddings');
    await store.stop('embeddings');
    expect(api['stopLocalModel']).toHaveBeenCalledWith('embeddings');
  });

  it('reports busy while starting or downloading', async () => {
    api['getRuntimeStatus']!.mockResolvedValueOnce(status('starting'));
    await store.refresh();
    expect(store.busy()).toBe(true);

    api['getRuntimeStatus']!.mockResolvedValueOnce(status('running', 'starting'));
    await store.refresh();
    expect(store.busy()).toBe(true);

    api['getRuntimeStatus']!.mockResolvedValueOnce(status('running', 'running'));
    await store.refresh();
    expect(store.busy()).toBe(false);

    api['getRuntimeStatus']!.mockResolvedValueOnce({
      ...status('stopped'),
      downloads: [
        {
          id: 'd',
          kind: 'model',
          label: 'm',
          receivedBytes: 1,
          totalBytes: 10,
          bytesPerSecond: 1,
          phase: 'downloading',
          error: null,
          startedAtMs: 0,
        },
      ],
    });
    await store.refresh();
    expect(store.busy()).toBe(true);
  });
});

import { TestBed } from '@angular/core/testing';
import type { HardwareInfo, RuntimeRelease, RuntimeStatus, ServerPhase, ServerState } from '@jarvis/shared-types';

import { JarvisApi } from '../ipc/jarvis-api';
import { ModelsStore } from './models.store';
import { RUNTIME_RELEASE_TIMEOUT_MS, RUNTIME_STATUS_TIMEOUT_MS, RuntimeStore, recommendedBackend } from './runtime.store';

function release(backend: 'cpu' | 'vulkan', tag = 'b1'): RuntimeRelease {
  return {
    backend,
    tag,
    publishedAt: '2026-10-23',
    assetName: 'runtime.zip',
    downloadUrl: 'https://example.com/runtime.zip',
    sizeBytes: 100,
    sha256: 'ab'.repeat(32),
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

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
    vision: false,
    toolCalling: false,
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
    vi.useFakeTimers();
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

  afterEach(() => {
    TestBed.resetTestingModule();
    vi.useRealTimers();
  });

  it.each(['success', 'failure'])('ignores stale release %s after changing builds', async (outcome) => {
    const old = deferred<RuntimeRelease>();
    const current = deferred<RuntimeRelease>();
    api['checkRuntimeRelease'] = vi.fn().mockReturnValueOnce(old.promise).mockReturnValueOnce(current.promise);
    const cpu = store.checkRelease('cpu');
    expect(store.checkRelease('cpu')).toBe(cpu);
    store.clearRelease();
    const vulkan = store.checkRelease('vulkan');
    if (outcome === 'success') old.resolve(release('cpu'));
    else old.reject(new Error('Old release failure'));
    await cpu;
    expect(store.release()).toBeNull();
    expect(store.actionErrors()).toEqual([]);
    expect(store.checkingRelease()).toBe(true);
    current.resolve(release('vulkan'));
    await vulkan;
    expect(store.release()?.backend).toBe('vulkan');
    expect(store.checkingRelease()).toBe(false);
    expect(api['checkRuntimeRelease']).toHaveBeenCalledTimes(2);
  });

  it('ignores an older CPU response after CPU to Vulkan to CPU changes', async () => {
    const old = deferred<RuntimeRelease>();
    api['checkRuntimeRelease'] = vi
      .fn()
      .mockReturnValueOnce(old.promise)
      .mockResolvedValueOnce(release('vulkan'))
      .mockResolvedValueOnce(release('cpu', 'b2'));
    const first = store.checkRelease('cpu');
    await store.checkRelease('vulkan');
    await store.checkRelease('cpu');
    old.resolve(release('cpu', 'b1'));
    await first;
    expect(store.release()?.tag).toBe('b2');
  });

  it('recovers from a release timeout without accepting its late result', async () => {
    const old = deferred<RuntimeRelease>();
    api['checkRuntimeRelease'] = vi.fn().mockReturnValueOnce(old.promise).mockResolvedValueOnce(release('cpu', 'b2'));
    const first = store.checkRelease('cpu');
    await vi.advanceTimersByTimeAsync(RUNTIME_RELEASE_TIMEOUT_MS);
    await first;
    expect(store.checkingRelease()).toBe(false);
    expect(store.actionErrors()[0]?.error.code).toBe('timeout');
    await store.checkRelease('cpu');
    old.resolve(release('cpu', 'b1'));
    await vi.advanceTimersByTimeAsync(0);
    expect(store.release()?.tag).toBe('b2');
    expect(store.actionErrors()).toEqual([]);
  });

  it('does not publish a release after the store is destroyed', async () => {
    const pending = deferred<RuntimeRelease>();
    api['checkRuntimeRelease'] = vi.fn().mockReturnValueOnce(pending.promise);
    const checking = store.checkRelease('cpu');
    TestBed.resetTestingModule();
    await checking;
    pending.resolve(release('cpu'));
    await vi.advanceTimersByTimeAsync(0);
    expect(store.release()).toBeNull();
    expect(store.actionErrors()).toEqual([]);
    expect(vi.getTimerCount()).toBe(0);
  });

  it('keeps a newer release result when an earlier installation finishes', async () => {
    const installing = deferred<void>();
    const first = release('cpu');
    store.release.set(first);
    api['installRuntime'] = vi.fn().mockReturnValueOnce(installing.promise);
    const install = store.install(first);
    api['checkRuntimeRelease'] = vi.fn().mockResolvedValueOnce(release('vulkan'));
    await store.checkRelease('vulkan');
    installing.resolve();
    await install;
    expect(store.release()?.backend).toBe('vulkan');
  });

  it.each(['success', 'failure'])('recovers after timeout and ignores late %s', async (outcome) => {
    await store.refresh();
    const pending = deferred<RuntimeStatus>();
    api['getRuntimeStatus']!.mockReturnValueOnce(pending.promise);
    const first = store.refresh();
    expect(store.refresh()).toBe(first);
    await vi.advanceTimersByTimeAsync(RUNTIME_STATUS_TIMEOUT_MS);
    await first;
    expect(store.statusError()?.code).toBe('timeout');
    api['getRuntimeStatus']!.mockResolvedValueOnce(status('running'));
    await store.refresh();
    expect(store.statusError()).toBeNull();
    expect(refreshModels).toHaveBeenCalledTimes(1);
    if (outcome === 'success') pending.resolve(status('stopped'));
    else pending.reject(new Error('Late native failure'));
    await vi.advanceTimersByTimeAsync(0);
    expect(store.server()?.phase).toBe('running');
    expect(refreshModels).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(0);
  });

  it('keeps an action failure visible after status recovery', async () => {
    api['getRuntimeStatus']!.mockRejectedValueOnce(new Error('Status unavailable'));
    await store.removeModel('m', true);
    expect(store.actionErrors()[0]?.error.code).toBe('permissionDenied');
    expect(store.statusError()?.message).toBe('Status unavailable');
    await store.refresh();
    expect(store.statusError()).toBeNull();
    expect(store.actionErrors()[0]?.error.code).toBe('permissionDenied');
  });

  it('coalesces commands for one server until its status refresh completes', async () => {
    const action = deferred<void>();
    const snapshot = deferred<RuntimeStatus>();
    api['startLocalModel']!.mockReturnValueOnce(action.promise);
    api['getRuntimeStatus']!.mockReturnValueOnce(snapshot.promise);
    const first = store.start('m', 'chat');
    expect(store.start('m', 'chat')).toBe(first);
    expect(store.stop('chat')).toBe(first);
    expect(api['startLocalModel']).toHaveBeenCalledTimes(1);
    expect(api['stopLocalModel']).not.toHaveBeenCalled();
    expect(store.isPending('server:chat')).toBe(true);
    action.resolve();
    await vi.advanceTimersByTimeAsync(0);
    expect(store.isPending('server:chat')).toBe(true);
    snapshot.resolve(status('running'));
    await first;
    expect(store.isPending('server:chat')).toBe(false);
    await store.stop('chat');
    expect(api['stopLocalModel']).toHaveBeenCalledTimes(1);
  });

  it('allows an independent server command and releases failed commands for retry', async () => {
    const action = deferred<void>();
    api['startLocalModel']!.mockReturnValueOnce(action.promise);
    const chat = store.start('m', 'chat');
    await store.start('bge', 'embeddings');
    expect(api['startLocalModel']).toHaveBeenCalledTimes(2);
    expect(store.isPending('server:chat')).toBe(true);
    expect(store.isPending('server:embeddings')).toBe(false);
    action.reject(new Error('Startup failed'));
    await chat;
    expect(store.actionErrors()[0]?.error.message).toBe('Startup failed');
    expect(store.isPending('server:chat')).toBe(false);
    await store.start('m', 'chat');
    expect(api['startLocalModel']).toHaveBeenCalledTimes(3);
  });

  it('prevents duplicate model removal including a conflicting delete request', async () => {
    const action = deferred<void>();
    api['removeModel']!.mockReturnValueOnce(action.promise);
    const first = store.removeModel('m', false);
    expect(store.removeModel('m', true)).toBe(first);
    expect(api['removeModel']).toHaveBeenCalledTimes(1);
    expect(api['removeModel']).toHaveBeenCalledWith('m', false);
    action.resolve();
    await first;
    expect(store.isPending('model:m')).toBe(false);
  });

  it('keeps concurrent command outcomes and errors independent', async () => {
    const download = deferred<string>();
    api['downloadModel'] = vi.fn().mockReturnValueOnce(download.promise);
    const pending = store.downloadModel({ repo: 'owner/repo', file: 'model.gguf', revision: null });
    expect(await store.removeModel('m', true)).toBe(false);
    download.resolve('download-id');
    expect(await pending).toBe(true);
    expect(store.actionErrors().map((failure) => failure.key)).toEqual(['model:m']);

    api['startLocalModel']!.mockRejectedValueOnce(new Error('Server unavailable'));
    expect(await store.start('m', 'chat')).toBe(false);
    expect(store.actionErrors().map((failure) => failure.key)).toEqual(['model:m', 'server:chat']);
    expect(await store.start('m', 'chat')).toBe(true);
    expect(store.actionErrors().map((failure) => failure.key)).toEqual(['model:m']);
    store.dismissError('model:m');
    expect(store.actionErrors()).toEqual([]);
  });

  it('does not lose a failed command result when another action starts during its status refresh', async () => {
    const snapshot = deferred<RuntimeStatus>();
    api['getRuntimeStatus']!.mockReturnValueOnce(snapshot.promise);
    api['downloadModel'] = vi.fn().mockRejectedValueOnce(new Error('Invalid repository'));
    const download = store.downloadModel({ repo: 'owner/repo', file: 'model.gguf', revision: null });
    await vi.advanceTimersByTimeAsync(0);
    const starting = store.start('m', 'chat');
    snapshot.resolve(status('running'));
    expect(await download).toBe(false);
    expect(await starting).toBe(true);
    expect(store.actionErrors()[0]?.error.message).toBe('Invalid repository');
  });

  it('returns command success even when the following status read fails', async () => {
    api['getRuntimeStatus']!.mockRejectedValueOnce(new Error('Status unavailable'));
    expect(await store.start('m', 'chat')).toBe(true);
    expect(store.actionErrors()).toEqual([]);
    expect(store.statusError()?.message).toBe('Status unavailable');
  });

  it('opens only one import dialog and shares its completion', async () => {
    const action = deferred<void>();
    api['importModel'] = vi.fn().mockReturnValueOnce(action.promise);
    const first = store.importModel();
    expect(store.importModel()).toBe(first);
    expect(api['importModel']).toHaveBeenCalledTimes(1);
    expect(store.isPending('import')).toBe(true);
    action.resolve();
    await first;
    expect(store.isPending('import')).toBe(false);
  });

  it('blocks catalog downloads while background transfer or verification is active', async () => {
    store.curated.set([
      {
        id: 'q',
        displayName: 'Q model',
        repo: 'owner/repo',
        file: 'q.gguf',
        revision: 'commit',
        sha256: 'hash',
        sizeBytes: 100,
        license: 'MIT',
        description: '',
        role: 'chat',
        projector: null,
      },
    ]);
    api['downloadCuratedModel'] = vi.fn(async () => 'new-download');
    for (const label of ['Q model', 'owner/repo / q.gguf']) {
      for (const phase of ['downloading', 'verifying', 'installing'] as const) {
        store.status.set({
          ...status('stopped'),
          downloads: [
            {
              id: 'd',
              kind: 'model',
              label,
              phase,
              receivedBytes: 1,
              totalBytes: 100,
              bytesPerSecond: 1,
              error: null,
              startedAtMs: 0,
            },
          ],
        });
        expect(store.isCuratedDownloading('q')).toBe(true);
        await store.downloadCurated('q');
      }
    }
    expect(api['downloadCuratedModel']).not.toHaveBeenCalled();
    for (const phase of ['cancelled', 'failed', 'completed'] as const) {
      store.status.set({
        ...status('stopped'),
        downloads: [
          {
            id: 'd',
            kind: 'model',
            label: 'Q model',
            phase,
            receivedBytes: 1,
            totalBytes: 100,
            bytesPerSecond: 0,
            error: null,
            startedAtMs: 0,
          },
        ],
      });
      expect(store.isCuratedDownloading('q')).toBe(false);
    }
    await store.downloadCurated('q');
    expect(api['downloadCuratedModel']).toHaveBeenCalledTimes(1);
  });

  it('reads a new snapshot after an action instead of keeping the earlier poll result', async () => {
    const pending = deferred<RuntimeStatus>();
    api['getRuntimeStatus']!.mockReturnValueOnce(pending.promise).mockResolvedValueOnce(status('running'));
    const poll = store.refresh();
    const starting = store.start('m', 'chat');
    await vi.advanceTimersByTimeAsync(0);
    pending.resolve(status('stopped'));
    await Promise.all([poll, starting]);
    expect(api['getRuntimeStatus']).toHaveBeenCalledTimes(2);
    expect(store.server()?.phase).toBe('running');
  });

  it('settles on destruction without publishing a late status', async () => {
    const pending = deferred<RuntimeStatus>();
    api['getRuntimeStatus']!.mockReturnValueOnce(pending.promise);
    const refreshing = store.refresh();
    TestBed.resetTestingModule();
    await refreshing;
    pending.resolve(status('running'));
    await vi.advanceTimersByTimeAsync(0);
    await store.refresh();
    expect(store.status()).toBeNull();
    expect(store.statusError()).toBeNull();
    expect(refreshModels).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
  });

  it('recommends Vulkan only with a Vulkan loader and a GPU with 4 GB or more', () => {
    const gpu = HARDWARE.gpus[0];
    const discrete = { ...HARDWARE, gpus: [{ ...gpu, vramTotalBytes: 8 * 1024 ** 3 }] };
    expect(recommendedBackend(discrete)).toBe('vulkan');
    expect(recommendedBackend({ ...discrete, vulkanAvailable: false })).toBe('cpu');
    // A small integrated GPU (Radeon Vega 8: 2 GB) or unknown memory: CPU.
    expect(recommendedBackend(HARDWARE)).toBe('cpu');
    expect(recommendedBackend({ ...HARDWARE, gpus: [{ ...gpu, vramTotalBytes: null }] })).toBe('cpu');
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

  it.each(['chat', 'embeddings'] as const)(
    'refreshes a %s model already running in the first snapshot only once',
    async (role) => {
      const running = role === 'chat' ? status('running') : status('stopped', 'running');
      api['getRuntimeStatus']!.mockResolvedValue(running);
      await store.refresh();
      expect(refreshModels).toHaveBeenCalledOnce();
      expect(refreshModels).toHaveBeenLastCalledWith(true);
      await store.refresh();
      expect(refreshModels).toHaveBeenCalledOnce();
    }
  );

  it.each(['chat', 'embeddings'] as const)(
    'refreshes changed %s model identity or capabilities without an observed phase change',
    async (role) => {
      const field = role === 'chat' ? 'server' : 'embeddingServer';
      const running = role === 'chat' ? status('running') : status('stopped', 'running');
      const changes: Partial<ServerState>[] = [
        { modelId: 'replacement-model' },
        { port: 32123 },
        { contextSize: 8192 },
        { vision: true },
        { toolCalling: true },
      ];
      for (const change of changes) {
        api['getRuntimeStatus']!.mockResolvedValueOnce(running);
        await store.refresh();
        refreshModels.mockClear();
        api['getRuntimeStatus']!.mockResolvedValueOnce({ ...running, [field]: { ...running[field], ...change } });
        await store.refresh();
        expect(refreshModels).toHaveBeenCalledOnce();
        expect(refreshModels).toHaveBeenLastCalledWith(true);
      }
    }
  );

  it('does not refresh models for runtime memory, uptime, or hardware changes', async () => {
    api['getRuntimeStatus']!.mockResolvedValueOnce(status('running', 'running'));
    await store.refresh();
    refreshModels.mockClear();
    api['getRuntimeStatus']!.mockResolvedValueOnce({
      ...status('running', 'running'),
      server: { ...server('running', 'qwen'), memoryBytes: 1024, startedAtMs: 1234, gpuLayers: 20, threads: 8 },
      embeddingServer: { ...server('running', 'bge-m3-q8_0'), memoryBytes: 2048, startedAtMs: 1235, gpuLayers: 10, threads: 4 },
    });
    await store.refresh();
    expect(refreshModels).not.toHaveBeenCalled();
  });

  it('records action errors and still refreshes the status', async () => {
    await store.removeModel('m', true);
    expect(store.actionErrors()[0]?.error.code).toBe('permissionDenied');
    expect(api['getRuntimeStatus']).toHaveBeenCalled();

    await store.start('m', 'chat');
    expect(store.actionErrors()).toHaveLength(1);
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

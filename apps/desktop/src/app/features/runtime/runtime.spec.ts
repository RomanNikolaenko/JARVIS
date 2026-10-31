import type {
  DownloadStatus,
  LocalModel,
  PoolingType,
  RuntimeStatus,
  ServerRole,
  ServerState,
  Settings,
} from '@jarvis/shared-types';
import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import { JarvisApi } from '../../core/ipc/jarvis-api';
import { IpcError } from '../../core/ipc/ipc-error';
import { RuntimeStore } from '../../core/state/runtime.store';
import { SettingsStore } from '../../core/state/settings.store';
import { SETTINGS } from '../../testing/settings.fixture';
import { RuntimePage, describeSource, downloadPercent, isEmbeddingModel, phaseBadge, roleOf } from './runtime';

class RuntimeFormHarness extends RuntimePage {
  readonly form = this.customDownload;
  readonly runtimeForm = this.settingsForm;
  readonly saved = this.settingsSaved;
  saveRuntime(): Promise<void> {
    return this.saveSettings();
  }
  readonly selectedBackend = this.backend;
  chooseBackend(value: string): void {
    const select = document.createElement('select');
    select.add(new Option(value, value));
    select.addEventListener('change', (event) => this.onBackend(event));
    select.dispatchEvent(new Event('change'));
  }
  submit(): void {
    this.downloadCustom();
  }
}

describe('Runtime custom download form', () => {
  let page: RuntimeFormHarness;
  let complete: (succeeded: boolean) => void;
  let download: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    download = vi.fn(
      () =>
        new Promise<boolean>((resolve) => {
          complete = resolve;
        })
    );
    TestBed.configureTestingModule({
      providers: [
        {
          provide: RuntimeStore,
          useValue: {
            loadStatic: vi.fn(async () => undefined),
            clearRelease: vi.fn(),
            refresh: vi.fn(async () => undefined),
            hardware: signal(null),
            settings: signal(null),
            savingSettings: signal(false),
            saveSettings: vi.fn(),
            isPending: vi.fn(() => false),
            downloadModel: download,
          },
        },
      ],
    });
    page = TestBed.runInInjectionContext(() => new RuntimeFormHarness());
    page.form.setValue({ repo: 'owner/repo', file: 'model.gguf', revision: '' });
  });

  afterEach(() => TestBed.resetTestingModule());

  it('saves and clears native tool model choices without enabling other models', async () => {
    const store = TestBed.inject(RuntimeStore);
    vi.mocked(store.saveSettings).mockResolvedValue(true);
    page.runtimeForm.controls.nativeToolModels.setValue(['qwen-tools']);
    await page.saveRuntime();
    expect(store.saveSettings).toHaveBeenLastCalledWith(expect.objectContaining({ nativeToolModels: ['qwen-tools'] }));
    page.runtimeForm.controls.nativeToolModels.setValue([]);
    await page.saveRuntime();
    expect(store.saveSettings).toHaveBeenLastCalledWith(expect.objectContaining({ nativeToolModels: [] }));
  });

  it.each([true, false])('resets submitted values only on command success (%s)', async (succeeded) => {
    page.submit();
    expect(download).toHaveBeenCalledWith({ repo: 'owner/repo', file: 'model.gguf', revision: null });
    complete(succeeded);
    await Promise.resolve();
    expect(page.form.controls.file.value).toBe(succeeded ? '' : 'model.gguf');
  });

  it('preserves new edits made while the previous request is completing', async () => {
    page.submit();
    page.form.controls.file.setValue('next.gguf');
    complete(true);
    await Promise.resolve();
    expect(page.form.controls.file.value).toBe('next.gguf');
  });

  it.each([true, false])('keeps edits made during settings save (%s)', async (succeeded) => {
    let finish!: (value: boolean) => void;
    const store = TestBed.inject(RuntimeStore);
    vi.mocked(store.saveSettings).mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      })
    );
    page.runtimeForm.markAsDirty();
    const saving = page.saveRuntime();
    await page.saveRuntime();
    expect(store.saveSettings).toHaveBeenCalledTimes(1);
    page.runtimeForm.controls.contextSize.setValue(8192);
    finish(succeeded);
    await saving;
    expect(page.runtimeForm.dirty).toBe(true);
    expect(page.runtimeForm.controls.contextSize.value).toBe(8192);
    expect(page.saved()).toBe(false);
  });

  it('marks unchanged settings pristine only after a successful save', async () => {
    vi.mocked(TestBed.inject(RuntimeStore).saveSettings).mockResolvedValueOnce(true);
    page.runtimeForm.markAsDirty();
    await page.saveRuntime();
    expect(page.runtimeForm.pristine).toBe(true);
    expect(page.saved()).toBe(true);
  });

  it('invalidates release checks when the user selects another backend', () => {
    const store = TestBed.inject(RuntimeStore);
    vi.mocked(store.clearRelease).mockClear();
    page.chooseBackend('vulkan');
    expect(page.selectedBackend()).toBe('vulkan');
    expect(store.clearRelease).toHaveBeenCalledTimes(1);
  });

  it('keeps a manual backend selection when hardware detection completes', () => {
    page.chooseBackend('vulkan');
    TestBed.inject(RuntimeStore).hardware.set({
      cpuBrand: 'CPU',
      physicalCores: 4,
      logicalCpus: 8,
      cpuFeatures: { avx: true, avx2: true, avx512f: false, fma: true, neon: false },
      totalMemoryBytes: 16e9,
      availableMemoryBytes: 8e9,
      gpus: [],
      vulkanAvailable: false,
      nvidiaDriver: null,
    });
    TestBed.tick();
    expect(page.selectedBackend()).toBe('vulkan');
  });
});

function model(pooling: PoolingType | null): LocalModel {
  return {
    id: 'm',
    displayName: 'M',
    path: '/models/m.gguf',
    sizeBytes: 1,
    sha256: null,
    source: { kind: 'imported' },
    gguf: {
      architecture: 'bert',
      name: null,
      sizeLabel: null,
      quantization: null,
      contextLength: 8192,
      blockCount: 24,
      embeddingLength: 1024,
      headCount: 16,
      headCountKv: null,
      pooling,
    },
    addedAtMs: 0,
    managed: true,
    available: true,
    projector: null,
  };
}

describe('Runtime model library', () => {
  async function render(overrides: Partial<RuntimeStatus> = {}) {
    const stopped: ServerState = {
      phase: 'stopped',
      modelId: null,
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
    const status: RuntimeStatus = {
      binary: null,
      server: stopped,
      embeddingServer: stopped,
      models: [model(null)],
      downloads: [],
      modelsDir: '/models',
      logTail: [],
      embeddingLogTail: [],
      ...overrides,
    };
    const api = {
      getRuntimeStatus: vi.fn(async () => status),
      getHardwareInfo: vi.fn(async () => null),
      listCuratedModels: vi.fn(async () => []),
      checkRuntimeRelease: vi.fn(),
      installRuntime: vi.fn(),
      selectRuntimeBinary: vi.fn(),
      startLocalModel: vi.fn(async (): Promise<void> => undefined),
      stopLocalModel: vi.fn(async (): Promise<void> => undefined),
      listAiModels: vi.fn(async () => []),
      getModelRoutes: vi.fn(async () => []),
      updateSettings: vi.fn(async (settings: Settings) => settings),
    };
    TestBed.configureTestingModule({
      imports: [RuntimePage],
      providers: [{ provide: JarvisApi, useValue: api }],
    });
    TestBed.inject(SettingsStore).settings.set(structuredClone(SETTINGS));
    const fixture = TestBed.createComponent(RuntimePage);
    await fixture.whenStable();
    fixture.detectChanges();
    return { fixture, element: fixture.nativeElement as HTMLElement, store: TestBed.inject(RuntimeStore), api };
  }

  afterEach(() => TestBed.resetTestingModule());

  it('explains how to install the missing engine beside models whose Start button is disabled', async () => {
    const { element, api } = await render();
    const notice = element.querySelector('.library .engine-required');
    expect(notice?.textContent).toContain('Install the engine to start your models.');
    expect(notice?.textContent).toContain('Find build');
    expect(notice?.textContent).toContain('Download & install');
    expect(notice?.textContent).toContain('Use existing llama-server');
    expect(notice?.querySelector('a')?.getAttribute('href')).toBe('#runtime-engine');
    expect(element.querySelector('#runtime-engine')).not.toBeNull();
    const engineActions = element.querySelector<HTMLElement>('.engine-actions')!;
    const focusEngine = vi.spyOn(engineActions, 'focus');
    notice?.querySelector<HTMLAnchorElement>('a')?.click();
    expect(focusEngine).toHaveBeenCalledOnce();
    expect(element.querySelector<HTMLButtonElement>('.library button.primary')?.disabled).toBe(true);
    expect(api.checkRuntimeRelease).not.toHaveBeenCalled();
    expect(api.installRuntime).not.toHaveBeenCalled();
    expect(api.selectRuntimeBinary).not.toHaveBeenCalled();
    expect(api.startLocalModel).not.toHaveBeenCalled();
  });

  it('removes the installation guidance and enables Start when the engine becomes available', async () => {
    const { fixture, element, store } = await render();
    store.status.update((status) => ({
      ...status!,
      binary: {
        path: '/runtime/llama-server',
        version: 'b1',
        backend: 'cpu',
        source: { kind: 'userSelected' },
        sha256: 'ab'.repeat(32),
        libraries: [],
        installedAtMs: 0,
      },
    }));
    fixture.detectChanges();
    expect(element.querySelector('.engine-required')).toBeNull();
    expect(element.querySelector<HTMLButtonElement>('.library button.primary')?.disabled).toBe(false);
  });

  function running(role: ServerRole): ServerState {
    return {
      phase: 'running',
      modelId: role === 'chat' ? 'chat-model' : 'embedding-model',
      port: role === 'chat' ? 12345 : 12346,
      pid: role === 'chat' ? 11 : 12,
      startedAtMs: 1,
      contextSize: 4096,
      gpuLayers: 0,
      threads: 4,
      memoryBytes: 1024,
      error: null,
      vision: false,
      toolCalling: false,
    };
  }

  function serving(role: ServerRole): Partial<RuntimeStatus> {
    return {
      binary: {
        path: '/runtime/llama-server',
        version: 'b1',
        backend: 'cpu',
        source: { kind: 'userSelected' },
        sha256: 'ab'.repeat(32),
        libraries: [],
        installedAtMs: 0,
      },
      [role === 'chat' ? 'server' : 'embeddingServer']: running(role),
    };
  }

  function serverButton(element: HTMLElement, role: ServerRole, label: string): HTMLButtonElement {
    const section = element.querySelector(`section[aria-label="${role === 'chat' ? 'Chat model' : 'Embedding model'}"]`)!;
    const button = [...section.querySelectorAll('button')].find((item) => item.textContent?.trim() === label);
    expect(button, `${role}: ${label}`).toBeDefined();
    return button!;
  }

  it.each(['chat', 'embeddings'] as const)('restarts only the running %s model on an explicit click', async (role) => {
    const { element, api } = await render(serving(role));
    const restart = serverButton(element, role, 'Restart');
    expect(restart.disabled).toBe(false);
    expect(api.startLocalModel).not.toHaveBeenCalled();
    restart.click();
    await vi.waitFor(() => expect(api.startLocalModel).toHaveBeenCalledOnce());
    expect(api.startLocalModel).toHaveBeenCalledWith(running(role).modelId, role);
    expect(api.stopLocalModel).not.toHaveBeenCalled();
    expect(api.updateSettings).not.toHaveBeenCalled();
    expect(api.installRuntime).not.toHaveBeenCalled();
  });

  it('blocks duplicate restart clicks while the server command is pending', async () => {
    const { element, api, store, fixture } = await render(serving('chat'));
    let finish!: () => void;
    api.startLocalModel.mockReturnValueOnce(
      new Promise<void>((resolve) => {
        finish = resolve;
      })
    );
    const restart = serverButton(element, 'chat', 'Restart');
    restart.click();
    restart.click();
    expect(api.startLocalModel).toHaveBeenCalledOnce();
    fixture.detectChanges();
    expect(restart.disabled).toBe(true);
    finish();
    await vi.waitFor(() => expect(store.isPending('server:chat')).toBe(false));
  });

  it.each(['starting', 'stopped', 'model', 'process', 'status-error'] as const)(
    'refuses a stale restart click after the live %s changes',
    async (change) => {
      const { element, api, store } = await render(serving('chat'));
      const restart = serverButton(element, 'chat', 'Restart');
      if (change === 'status-error') {
        store.statusError.set(new IpcError('unavailable', 'Status unavailable'));
      } else {
        store.status.update((status) => ({
          ...status!,
          server: {
            ...status!.server,
            ...(change === 'starting' || change === 'stopped' ? { phase: change } : {}),
            ...(change === 'model' ? { modelId: 'another-model' } : {}),
            ...(change === 'process' ? { pid: 99, port: 12399 } : {}),
          },
        }));
      }
      // No detectChanges: exercise the old button's handler against current state.
      restart.click();
      expect(api.startLocalModel).not.toHaveBeenCalled();
    }
  );

  it('keeps Stop available while starting without offering Restart', async () => {
    const { element, api } = await render({ ...serving('chat'), server: { ...running('chat'), phase: 'starting' } });
    const section = element.querySelector('section[aria-label="Chat model"]')!;
    expect([...section.querySelectorAll('button')].some((item) => item.textContent?.trim() === 'Restart')).toBe(false);
    serverButton(element, 'chat', 'Stop').click();
    await vi.waitFor(() => expect(api.stopLocalModel).toHaveBeenCalledWith('chat'));
    expect(api.startLocalModel).not.toHaveBeenCalled();
  });

  it('requires saving inference edits before an explicit restart and never saves or starts automatically', async () => {
    const { element, api, fixture } = await render(serving('chat'));
    const restart = serverButton(element, 'chat', 'Restart');
    const context = element.querySelector<HTMLInputElement>('input[formControlName="contextSize"]')!;
    context.value = '8192';
    context.dispatchEvent(new Event('input'));
    // Even an old enabled button must refuse unsaved changes.
    restart.click();
    expect(api.startLocalModel).not.toHaveBeenCalled();
    expect(api.updateSettings).not.toHaveBeenCalled();
    fixture.detectChanges();
    expect(restart.disabled).toBe(true);
    expect(element.querySelector('.server .hint')?.textContent).toContain('Save inference changes before restarting');
    const save = [...element.querySelectorAll('button')].find((item) => item.textContent?.trim() === 'Save')!;
    save.click();
    await vi.waitFor(() => {
      fixture.detectChanges();
      expect(restart.disabled).toBe(false);
    });
    expect(api.updateSettings).toHaveBeenCalledOnce();
    expect(api.updateSettings).toHaveBeenCalledWith(
      expect.objectContaining({ runtime: expect.objectContaining({ contextSize: 8192 }) })
    );
    expect(api.startLocalModel).not.toHaveBeenCalled();
    restart.click();
    await vi.waitFor(() => expect(api.startLocalModel).toHaveBeenCalledWith('chat-model', 'chat'));
  });

  it('blocks restarting during settings or engine operations while keeping Stop available', async () => {
    const { element, api, fixture, store } = await render(serving('chat'));
    const restart = serverButton(element, 'chat', 'Restart');
    const settings = TestBed.inject(SettingsStore);
    settings.saving.set(true);
    fixture.detectChanges();
    expect(restart.disabled).toBe(true);
    expect(serverButton(element, 'chat', 'Stop').disabled).toBe(false);
    settings.saving.set(false);
    let finish!: () => void;
    api.selectRuntimeBinary.mockReturnValueOnce(
      new Promise<void>((resolve) => {
        finish = resolve;
      })
    );
    const selecting = store.selectBinary();
    fixture.detectChanges();
    expect(restart.disabled).toBe(true);
    expect(serverButton(element, 'chat', 'Stop').disabled).toBe(false);
    finish();
    await selecting;
    store.status.update((status) => ({
      ...status!,
      downloads: [
        {
          id: 'engine-install',
          kind: 'runtime',
          label: 'llama.cpp',
          phase: 'installing',
          receivedBytes: 1,
          totalBytes: 1,
          bytesPerSecond: 0,
          error: null,
          startedAtMs: 1,
        },
      ],
    }));
    fixture.detectChanges();
    expect(restart.disabled).toBe(true);
    expect(serverButton(element, 'chat', 'Stop').disabled).toBe(false);
    expect(api.startLocalModel).not.toHaveBeenCalled();
  });
});

describe('Runtime page helpers', () => {
  it('describes where the engine came from', () => {
    expect(describeSource({ kind: 'release', tag: 'b11734', asset: 'llama.zip', url: 'https://x' })).toBe(
      'Official llama.cpp b11734 (llama.zip)'
    );
    expect(describeSource({ kind: 'userSelected' })).toBe('Selected by you');
  });

  it('computes bounded download progress', () => {
    const download = (received: number, total: number | null): DownloadStatus => ({
      id: 'd',
      kind: 'model',
      label: 'm',
      receivedBytes: received,
      totalBytes: total,
      bytesPerSecond: 0,
      phase: 'downloading',
      error: null,
      startedAtMs: 0,
    });
    expect(downloadPercent(download(50, 200))).toBe(25);
    expect(downloadPercent(download(300, 200))).toBe(100);
    expect(downloadPercent(download(10, null))).toBe(0);
  });

  it('starts embedding models in the embedding server', () => {
    expect(isEmbeddingModel(model('cls'))).toBe(true);
    expect(roleOf(model('mean'))).toBe('embeddings');
    expect(roleOf(model('last'))).toBe('embeddings');
    // Chat models declare no pooling; rerankers produce no embeddings.
    expect(roleOf(model(null))).toBe('chat');
    expect(isEmbeddingModel(model('rank'))).toBe(false);
    expect(isEmbeddingModel(model('none'))).toBe(false);
  });

  it('maps server phases to badge statuses', () => {
    expect(phaseBadge('running')).toBe('healthy');
    expect(phaseBadge('failed')).toBe('unhealthy');
    expect(phaseBadge('stopped')).toBe('unknown');
    expect(phaseBadge('starting')).toBe('degraded');
  });
});

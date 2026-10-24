import type { DownloadStatus, LocalModel, PoolingType } from '@jarvis/shared-types';
import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import { RuntimeStore } from '../../core/state/runtime.store';
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
  };
}

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

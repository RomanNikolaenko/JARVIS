import type { DownloadStatus, LocalModel, PoolingType } from '@jarvis/shared-types';

import { describeSource, downloadPercent, isEmbeddingModel, phaseBadge, roleOf } from './runtime';

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

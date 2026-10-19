import type { DownloadStatus } from '@jarvis/shared-types';

import { describeSource, downloadPercent } from './runtime';

describe('Runtime page helpers', () => {
  it('describes where the engine came from', () => {
    expect(
      describeSource({ kind: 'release', tag: 'b11734', asset: 'llama.zip', url: 'https://x' }),
    ).toBe('Official llama.cpp b11734 (llama.zip)');
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
});

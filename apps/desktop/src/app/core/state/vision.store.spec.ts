import { TestBed } from '@angular/core/testing';
import type { CaptureInfo } from '@jarvis/shared-types';

import { JarvisApi } from '../ipc/jarvis-api';
import { MAX_ATTACHMENTS, VisionStore } from './vision.store';

function capture(id: string): CaptureInfo {
  return {
    id,
    source: 'screen',
    label: 'Display 1',
    width: 1568,
    height: 882,
    preview: 'data:image/jpeg;base64,AAAA',
    capturedAtMs: 1,
  };
}

describe('VisionStore', () => {
  let api: Record<string, ReturnType<typeof vi.fn>>;
  let store: VisionStore;
  let next: number;

  beforeEach(() => {
    next = 0;
    api = {
      captureScreen: vi.fn(async () => capture(`c${++next}`)),
      captureSelection: vi.fn(async () => {
        throw { code: 'cancelled', message: 'the capture was cancelled' };
      }),
      captureWindow: vi.fn(async () => {
        throw { code: 'notFound', message: 'the window was closed' };
      }),
      attachImageFile: vi.fn(async () => null),
      discardCapture: vi.fn(async () => true),
    };
    TestBed.configureTestingModule({ providers: [{ provide: JarvisApi, useValue: api }] });
    store = TestBed.inject(VisionStore);
  });

  it('collects captures for the next message and hands them over once', async () => {
    await store.captureScreen(null, true);
    await store.captureScreen('2', false);
    expect(api['captureScreen']).toHaveBeenCalledWith('2', false);
    expect(store.attachments().map((a) => a.id)).toEqual(['c1', 'c2']);
    expect(store.take().map((a) => a.id)).toEqual(['c1', 'c2']);
    expect(store.attachments()).toEqual([]);
  });

  it('discards removed captures in the core', async () => {
    await store.captureScreen(null);
    await store.remove('c1');
    expect(store.attachments()).toEqual([]);
    expect(api['discardCapture']).toHaveBeenCalledWith('c1');
  });

  it('stops at the attachment limit', async () => {
    for (let i = 0; i < MAX_ATTACHMENTS + 1; i++) {
      await store.captureScreen(null);
    }
    expect(store.attachments()).toHaveLength(MAX_ATTACHMENTS);
    expect(store.full()).toBe(true);
    expect(store.error()?.code).toBe('limitExceeded');
  });

  it('treats a closed system dialog as no capture, and reports real failures', async () => {
    await store.captureSelection();
    expect(store.error()).toBeNull();
    await store.attachFile();
    expect(store.attachments()).toEqual([]);
    await store.captureWindow('42');
    expect(store.error()?.message).toBe('the window was closed');
  });

  it('discards existing and late captures after changing the draft', async () => {
    await store.captureScreen(null);
    let finish!: (info: CaptureInfo) => void;
    api['captureScreen'].mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      })
    );
    const pending = store.captureScreen(null);
    store.clear();
    finish(capture('late'));
    await pending;
    expect(store.attachments()).toEqual([]);
    expect(api['discardCapture']).toHaveBeenCalledWith('c1');
    expect(api['discardCapture']).toHaveBeenCalledWith('late');
  });

  it('serializes capture requests so concurrent calls cannot exceed the limit', async () => {
    for (let i = 0; i < MAX_ATTACHMENTS - 1; i++) await store.captureScreen(null);
    let finish!: (info: CaptureInfo) => void;
    api['captureScreen'].mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      })
    );
    const pending = store.captureScreen(null);
    await store.attachFile();
    expect(api['attachImageFile']).not.toHaveBeenCalled();
    expect(store.busy()).toBe(true);
    finish(capture('last'));
    await pending;
    expect(store.attachments()).toHaveLength(MAX_ATTACHMENTS);
  });

  it('does not attach a pending capture to the next message after take', async () => {
    let finish!: (info: CaptureInfo) => void;
    api['captureScreen'].mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      })
    );
    const pending = store.captureScreen(null);
    store.take();
    finish(capture('late'));
    await pending;
    expect(store.attachments()).toEqual([]);
    expect(api['discardCapture']).toHaveBeenCalledWith('late');
  });

  it('discards a capture completed after destruction', async () => {
    let finish!: (info: CaptureInfo) => void;
    api['captureScreen'].mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      })
    );
    const pending = store.captureScreen(null);
    TestBed.resetTestingModule();
    finish(capture('late'));
    await pending;
    expect(store.attachments()).toEqual([]);
    expect(api['discardCapture']).toHaveBeenCalledWith('late');
    await store.captureScreen(null);
    expect(api['captureScreen']).toHaveBeenCalledTimes(1);
  });
  it('releases busy after a window-list timeout and allows retry', async () => {
    vi.useFakeTimers();
    try {
      api['listWindows'] = vi
        .fn()
        .mockReturnValueOnce(new Promise(() => undefined))
        .mockResolvedValueOnce([]);
      const first = store.loadWindows();
      await vi.advanceTimersByTimeAsync(10_001);
      await first;
      expect(store.busy()).toBe(false);
      expect(store.error()?.code).toBe('timeout');
      await store.loadWindows();
      expect(store.error()).toBeNull();
      expect(store.windows()).toEqual([]);
    } finally {
      vi.useRealTimers();
    }
  });
});

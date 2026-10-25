import { TestBed } from '@angular/core/testing';
import type { UpdateStatus } from '@jarvis/shared-types';

import { JarvisApi } from '../ipc/jarvis-api';
import { UpdatesStore } from './updates.store';

function status(overrides: Partial<UpdateStatus> = {}): UpdateStatus {
  return {
    currentVersion: '0.1.0',
    phase: 'idle',
    available: null,
    downloadedBytes: null,
    totalBytes: null,
    lastCheckedMs: null,
    error: null,
    ...overrides,
  };
}

describe('UpdatesStore', () => {
  let api: Record<string, ReturnType<typeof vi.fn>>;
  let pushed: ((status: UpdateStatus) => void) | undefined;
  let store: UpdatesStore;

  beforeEach(() => {
    localStorage.removeItem('jarvis.update-seen');
    api = {
      onUpdateStatus: vi.fn(async (handler: (status: UpdateStatus) => void) => {
        pushed = handler;
        return () => undefined;
      }),
      getUpdateStatus: vi.fn(async () => status()),
      checkForUpdate: vi.fn(async () =>
        status({ phase: 'available', available: { version: '0.2.0', notes: 'Fixes', date: null } })
      ),
      installUpdate: vi.fn(async () => undefined),
    };
    TestBed.configureTestingModule({ providers: [{ provide: JarvisApi, useValue: api }] });
    store = TestBed.inject(UpdatesStore);
  });

  it('shows a found update and installs only the confirmed version', async () => {
    await store.start();
    expect(store.available()).toBeNull();
    await store.check();
    expect(store.available()?.version).toBe('0.2.0');
    expect(api['installUpdate']).not.toHaveBeenCalled();
    await store.install('0.2.0');
    expect(api['installUpdate']).toHaveBeenCalledWith('0.2.0');
  });

  it('follows pushed progress', async () => {
    await store.start();
    pushed?.(status({ phase: 'downloading', downloadedBytes: 25, totalBytes: 100 }));
    expect(store.progressPercent()).toBe(25);
    expect(store.busy()).toBe(true);
    pushed?.(status({ phase: 'failed', error: 'The update check failed' }));
    expect(store.busy()).toBe(false);
  });

  it('reports failures', async () => {
    api['checkForUpdate'].mockRejectedValueOnce({ code: 'unavailable', message: 'offline' });
    await store.check();
    expect(store.error()?.message).toBe('offline');
  });

  it('installs the found version with one call and checks again after a failed attempt', async () => {
    await store.start();
    await store.update();
    expect(api['installUpdate']).not.toHaveBeenCalled();

    await store.check();
    await store.update();
    expect(api['installUpdate']).toHaveBeenLastCalledWith('0.2.0');

    // The download failed: the core dropped it, so the retry checks first.
    pushed?.(status({ phase: 'failed', available: { version: '0.2.0', notes: null, date: null }, error: 'bad signature' }));
    expect(store.retryVersion()).toBe('0.2.0');
    api['checkForUpdate'].mockClear();
    await store.update();
    expect(api['checkForUpdate']).toHaveBeenCalledTimes(1);
    expect(api['installUpdate']).toHaveBeenCalledTimes(2);
  });

  afterEach(() => localStorage.removeItem('jarvis.update-seen'));

  it('draws attention to a new version until it was looked at', async () => {
    await store.start();
    await store.check();
    expect(store.attention()).toBe(true);
    store.markSeen();
    expect(store.attention()).toBe(false);
    expect(localStorage.getItem('jarvis.update-seen')).toBe('0.2.0');

    pushed?.(status({ phase: 'available', available: { version: '0.3.0', notes: null, date: null } }));
    expect(store.attention()).toBe(true);
  });
});

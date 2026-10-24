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
});

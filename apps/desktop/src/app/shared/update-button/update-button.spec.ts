import { TestBed } from '@angular/core/testing';
import type { UpdateStatus } from '@jarvis/shared-types';

import { JarvisApi } from '../../core/ipc/jarvis-api';
import { UpdatesStore } from '../../core/state/updates.store';
import { UpdateButton } from './update-button';

function status(overrides: Partial<UpdateStatus> = {}): UpdateStatus {
  return {
    currentVersion: '0.1.0',
    phase: 'upToDate',
    available: null,
    downloadedBytes: null,
    totalBytes: null,
    lastCheckedMs: null,
    error: null,
    ...overrides,
  };
}

describe('UpdateButton', () => {
  let api: Record<string, ReturnType<typeof vi.fn>>;

  async function setup(initial: UpdateStatus) {
    localStorage.removeItem('jarvis.update-seen');
    api = {
      onUpdateStatus: vi.fn(async () => () => undefined),
      getUpdateStatus: vi.fn(async () => initial),
      checkForUpdate: vi.fn(async () => initial),
      installUpdate: vi.fn(async () => undefined),
    };
    TestBed.configureTestingModule({ imports: [UpdateButton], providers: [{ provide: JarvisApi, useValue: api }] });
    const store = TestBed.inject(UpdatesStore);
    await store.start();
    const fixture = TestBed.createComponent(UpdateButton);
    await fixture.whenStable();
    const button = () => fixture.nativeElement.querySelector('button') as HTMLButtonElement;
    return { fixture, store, button };
  }

  afterEach(() => localStorage.removeItem('jarvis.update-seen'));

  it('shows the running version and checks again on click', async () => {
    const { fixture, button } = await setup(status());
    expect(button().textContent?.trim()).toBe('v0.1.0');
    expect(button().title).toBe('JARVIS 0.1.0 is the latest version. Click to check again.');
    expect(button().classList).not.toContain('available');

    button().click();
    await fixture.whenStable();
    expect(api['checkForUpdate']).toHaveBeenCalledTimes(1);
    expect(api['installUpdate']).not.toHaveBeenCalled();
  });

  it('offers a found version, pulses until looked at, and installs it on click', async () => {
    const available = status({ phase: 'available', available: { version: '0.2.0', notes: 'Faster chat', date: null } });
    const { fixture, button } = await setup(available);
    expect(button().textContent?.trim()).toBe('Update to 0.2.0');
    expect(button().classList).toContain('available');
    expect(button().title).toContain('Download, verify and install JARVIS 0.2.0');
    expect(button().title).toContain('Faster chat');
    expect(fixture.nativeElement.querySelector('.dot')).not.toBeNull();

    button().dispatchEvent(new Event('pointerenter'));
    await fixture.whenStable();
    expect(fixture.nativeElement.querySelector('.dot')).toBeNull();

    button().click();
    await fixture.whenStable();
    expect(api['installUpdate']).toHaveBeenCalledWith('0.2.0');
  });

  it('shows the download progress and cannot be clicked meanwhile', async () => {
    const { button } = await setup(status({ phase: 'downloading', downloadedBytes: 45, totalBytes: 100 }));
    expect(button().textContent?.trim()).toBe('Updating… 45%');
    expect(button().disabled).toBe(true);
  });

  it('offers to try again after a failed download', async () => {
    const failed = status({
      phase: 'failed',
      available: { version: '0.2.0', notes: null, date: null },
      error: "The update's signature is not valid",
    });
    const { fixture, button } = await setup(failed);
    expect(button().textContent?.trim()).toBe('Retry update 0.2.0');
    expect(button().title).toBe("The update's signature is not valid. Click to try 0.2.0 again.");

    api['checkForUpdate'].mockResolvedValueOnce(
      status({ phase: 'available', available: { version: '0.2.0', notes: null, date: null } })
    );
    button().click();
    await fixture.whenStable();
    await fixture.whenStable();
    expect(api['checkForUpdate']).toHaveBeenCalledTimes(1);
    expect(api['installUpdate']).toHaveBeenCalledWith('0.2.0');
  });

  it('marks a failed check without an update', async () => {
    const { button } = await setup(status({ phase: 'failed', error: 'The update check failed: offline' }));
    expect(button().textContent?.trim()).toBe('v0.1.0');
    expect(button().classList).toContain('failed');
    expect(button().title).toBe('The update check failed: offline. Click to check again.');
  });
});

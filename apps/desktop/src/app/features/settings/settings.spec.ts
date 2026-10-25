import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import type { Settings } from '@jarvis/shared-types';

import { JarvisApi } from '../../core/ipc/jarvis-api';
import { SettingsStore } from '../../core/state/settings.store';
import { SystemStore } from '../../core/state/system.store';
import { SETTINGS } from '../../testing/settings.fixture';
import { SettingsPage } from './settings';

class SettingsHarness extends SettingsPage {
  readonly fields = this.form;
  readonly saved = this.savedAt;
  submit(): Promise<void> {
    return this.save();
  }
}

describe('Settings form', () => {
  afterEach(() => TestBed.resetTestingModule());

  it.each([true, false])('preserves unsaved edits made during a successful request (edited: %s)', async (edited) => {
    let finish!: (settings: Settings) => void;
    const updateSettings = vi.fn(
      () =>
        new Promise<Settings>((resolve) => {
          finish = resolve;
        })
    );
    TestBed.configureTestingModule({
      providers: [
        { provide: JarvisApi, useValue: { updateSettings } },
        { provide: SystemStore, useValue: { appInfo: signal(null) } },
      ],
    });
    TestBed.inject(SettingsStore).settings.set(SETTINGS);
    const page = TestBed.runInInjectionContext(() => new SettingsHarness());
    TestBed.tick();
    page.fields.controls.requestTimeoutSecs.setValue(120);
    page.fields.markAsDirty();
    const saving = page.submit();
    await page.submit();
    expect(updateSettings).toHaveBeenCalledTimes(1);
    if (edited) page.fields.controls.requestTimeoutSecs.setValue(180);
    finish({ ...SETTINGS, ai: { ...SETTINGS.ai, requestTimeoutSecs: 120 } });
    await saving;
    TestBed.tick();
    expect(page.fields.controls.requestTimeoutSecs.value).toBe(edited ? 180 : 120);
    expect(page.fields.dirty).toBe(edited);
    expect(page.saved() !== null).toBe(!edited);
  });

  it('installs an update only after the user confirms it, and saves the automatic check setting', async () => {
    const available = {
      currentVersion: '0.1.0',
      phase: 'available' as const,
      available: { version: '0.2.0', notes: 'Faster start-up', date: null },
      downloadedBytes: null,
      totalBytes: null,
      lastCheckedMs: 1,
      error: null,
    };
    const api = {
      onUpdateStatus: vi.fn(async () => () => undefined),
      getUpdateStatus: vi.fn(async () => available),
      installUpdate: vi.fn(async () => undefined),
      updateSettings: vi.fn(async (settings: Settings) => settings),
    };
    TestBed.configureTestingModule({
      imports: [SettingsPage],
      providers: [
        provideRouter([]),
        { provide: JarvisApi, useValue: api },
        { provide: SystemStore, useValue: { appInfo: signal(null) } },
      ],
    });
    TestBed.inject(SettingsStore).settings.set(SETTINGS);
    const fixture = TestBed.createComponent(SettingsPage);
    await fixture.whenStable();
    fixture.detectChanges();
    const element = fixture.nativeElement as HTMLElement;
    const button = (text: string) =>
      [...element.querySelectorAll('button')].find((b) => b.textContent?.trim().startsWith(text)) as HTMLButtonElement;

    expect(element.textContent).toContain('version 0.2.0 is available');
    expect(element.textContent).toContain('Faster start-up');
    button('Install 0.2.0').click();
    fixture.detectChanges();
    expect(api.installUpdate).not.toHaveBeenCalled();
    expect(element.textContent).toContain('installs version 0.2.0 and starts');
    button('Install and restart').click();
    await fixture.whenStable();
    expect(api.installUpdate).toHaveBeenCalledWith('0.2.0');

    const automatic = element.querySelector<HTMLInputElement>('input[formcontrolname="checkUpdates"]')!;
    expect(automatic.checked).toBe(true);
    automatic.click();
    fixture.detectChanges();
    button('Save settings').click();
    await fixture.whenStable();
    expect(api.updateSettings).toHaveBeenCalledWith(expect.objectContaining({ updates: { checkAutomatically: false } }));
  });
});

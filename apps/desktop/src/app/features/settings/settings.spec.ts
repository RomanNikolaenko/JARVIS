import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
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
});

import { TestBed } from '@angular/core/testing';
import type { Settings } from '@jarvis/shared-types';

import { SETTINGS } from '../../testing/settings.fixture';
import { JarvisApi } from '../ipc/jarvis-api';
import { SettingsStore } from './settings.store';

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

describe('SettingsStore', () => {
  let store: SettingsStore;
  let api: {
    getSettings: ReturnType<typeof vi.fn>;
    updateSettings: ReturnType<typeof vi.fn>;
    addWorkspaceFolder: ReturnType<typeof vi.fn>;
  };
  beforeEach(() => {
    api = {
      getSettings: vi.fn(async () => SETTINGS),
      updateSettings: vi.fn(async (settings: Settings) => settings),
      addWorkspaceFolder: vi.fn(async () => null),
    };
    TestBed.configureTestingModule({ providers: [{ provide: JarvisApi, useValue: api }] });
    store = TestBed.inject(SettingsStore);
    store.settings.set(SETTINGS);
  });
  afterEach(() => TestBed.resetTestingModule());

  it('blocks settings writes and duplicate pickers while a folder dialog is open', async () => {
    const pending = deferred<Settings | null>();
    api.addWorkspaceFolder.mockReturnValueOnce(pending.promise);
    const adding = store.addWorkspaceFolder();
    expect(store.saving()).toBe(true);
    expect(await store.save(SETTINGS)).toBe(false);
    expect(await store.addWorkspaceFolder()).toBe(false);
    expect(api.updateSettings).not.toHaveBeenCalled();
    expect(api.addWorkspaceFolder).toHaveBeenCalledTimes(1);
    const next = { ...SETTINGS, tools: { ...SETTINGS.tools, workspaceFolders: ['/new'] } };
    pending.resolve(next);
    expect(await adding).toBe(true);
    expect(store.settings()).toEqual(next);
    expect(store.saving()).toBe(false);
    expect(await store.save(next)).toBe(true);
  });

  it('does not open a folder dialog during another settings write', async () => {
    const pending = deferred<Settings>();
    api.updateSettings.mockReturnValueOnce(pending.promise);
    const saving = store.save(SETTINGS);
    expect(await store.addWorkspaceFolder()).toBe(false);
    expect(api.addWorkspaceFolder).not.toHaveBeenCalled();
    pending.resolve(SETTINGS);
    await saving;
  });

  it.each(['cancel', 'failure'])('releases the folder guard after %s without changing settings', async (outcome) => {
    if (outcome === 'failure') api.addWorkspaceFolder.mockRejectedValueOnce(new Error('Folder unavailable'));
    expect(await store.addWorkspaceFolder()).toBe(false);
    expect(store.settings()).toEqual(SETTINGS);
    expect(store.saving()).toBe(false);
    expect(store.error()?.message ?? null).toBe(outcome === 'failure' ? 'Folder unavailable' : null);
    expect(await store.save(SETTINGS)).toBe(true);
  });

  it('invalidates a settings read started before the folder picker', async () => {
    const old = deferred<Settings>();
    api.getSettings.mockReturnValueOnce(old.promise);
    const loading = store.load();
    const next = { ...SETTINGS, tools: { ...SETTINGS.tools, workspaceFolders: ['/new'] } };
    api.addWorkspaceFolder.mockResolvedValueOnce(next);
    expect(await store.addWorkspaceFolder()).toBe(true);
    old.resolve(SETTINGS);
    await loading;
    expect(store.settings()).toEqual(next);
  });

  it('rejects overlapping saves without reporting success or clearing the pending state', async () => {
    const pending = deferred<Settings>();
    api.updateSettings.mockReturnValueOnce(pending.promise);
    const first = store.save(SETTINGS);
    expect(await store.save({ ...SETTINGS, runtime: { ...SETTINGS.runtime, contextSize: 8192 } })).toBe(false);
    expect(api.updateSettings).toHaveBeenCalledTimes(1);
    expect(store.saving()).toBe(true);
    await store.load();
    expect(api.getSettings).not.toHaveBeenCalled();
    pending.resolve(SETTINGS);
    expect(await first).toBe(true);
    expect(store.saving()).toBe(false);
  });

  it.each(['success', 'failure'])('ignores an earlier load %s after a save', async (outcome) => {
    const pending = deferred<Settings>();
    api.getSettings.mockReturnValueOnce(pending.promise);
    const loading = store.load();
    const next = { ...SETTINGS, runtime: { ...SETTINGS.runtime, contextSize: 8192 } };
    expect(await store.save(next)).toBe(true);
    if (outcome === 'success') pending.resolve(SETTINGS);
    else pending.reject(new Error('Old load failed'));
    await loading;
    expect(store.settings()).toEqual(next);
    expect(store.error()).toBeNull();
  });

  it('retains settings on failure and permits an explicit retry', async () => {
    api.updateSettings.mockRejectedValueOnce(new Error('Cannot write settings'));
    expect(await store.save(SETTINGS)).toBe(false);
    expect(store.settings()).toEqual(SETTINGS);
    expect(store.error()?.message).toBe('Cannot write settings');
    expect(store.saving()).toBe(false);
    expect(await store.save(SETTINGS)).toBe(true);
    expect(store.error()).toBeNull();
  });

  it('does not publish a save response after destruction', async () => {
    const pending = deferred<Settings>();
    api.updateSettings.mockReturnValueOnce(pending.promise);
    const saving = store.save(SETTINGS);
    TestBed.resetTestingModule();
    pending.resolve({ ...SETTINGS, runtime: { ...SETTINGS.runtime, contextSize: 8192 } });
    expect(await saving).toBe(false);
    expect(store.settings()).toEqual(SETTINGS);
  });
});

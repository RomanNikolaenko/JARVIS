import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import type { Settings } from '@jarvis/shared-types';

import { JarvisApi } from '../core/ipc/jarvis-api';
import { AgentsStore } from '../core/state/agents.store';
import { MemoryStore } from '../core/state/memory.store';
import { ModelsStore } from '../core/state/models.store';
import { SettingsStore } from '../core/state/settings.store';
import { ToolsPanel } from '../features/agents/tools-panel/tools-panel';
import { DataPanel } from '../features/memory/data-panel/data-panel';
import { SETTINGS } from './settings.fixture';

class MemoryHarness extends DataPanel {
  edit(value: number): void {
    this.update({ episodeRetentionDays: value });
  }
  value(): number | undefined {
    return this.form()?.episodeRetentionDays;
  }
  isDirty(): boolean {
    return this.dirty();
  }
  isSaved(): boolean {
    return this.saved();
  }
  submit(): Promise<void> {
    return this.save();
  }
  reset(): void {
    this.discard();
  }
}

class ToolsHarness extends ToolsPanel {
  edit(value: number): void {
    this.setAgents('maxSteps', value);
  }
  value(): number | undefined {
    return this.draft()?.agents.maxSteps;
  }
  isDirty(): boolean {
    return this.dirty();
  }
  isSaved(): boolean {
    return this.savedAt() !== null;
  }
  submit(): Promise<void> {
    return this.save();
  }
  reset(): void {
    this.discard();
  }
  programs(value: string): void {
    this.setPrograms(value);
  }
  programValue(): string | undefined {
    return this.draft()?.programs;
  }
}

describe.each(['memory', 'tools'] as const)('%s settings save', (kind) => {
  let panel: MemoryHarness | ToolsHarness;
  let resolve: (settings: Settings) => void;
  let reject: (error: unknown) => void;
  let submitted: Settings;
  let updateSettings: ReturnType<typeof vi.fn>;
  let refreshStatus: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    updateSettings = vi.fn((settings: Settings) => {
      submitted = settings;
      return new Promise<Settings>((yes, no) => {
        resolve = yes;
        reject = no;
      });
    });
    refreshStatus = vi.fn(async () => undefined);
    TestBed.configureTestingModule({
      providers: [
        { provide: JarvisApi, useValue: { updateSettings } },
        { provide: MemoryStore, useValue: { refreshStatus } },
        { provide: ModelsStore, useValue: { refresh: vi.fn(async () => undefined), options: signal([]) } },
        { provide: AgentsStore, useValue: { refreshStatus, loadTools: vi.fn(async () => undefined) } },
      ],
    });
    TestBed.inject(SettingsStore).settings.set(structuredClone(SETTINGS));
    panel = TestBed.runInInjectionContext(() => (kind === 'memory' ? new MemoryHarness() : new ToolsHarness()));
    TestBed.tick();
    panel.edit(24);
  });

  afterEach(() => TestBed.resetTestingModule());

  it.each([true, false])('preserves the draft when edited during save (%s)', async (edited) => {
    const saving = panel.submit();
    await panel.submit();
    expect(updateSettings).toHaveBeenCalledTimes(1);
    panel.reset();
    expect(panel.value()).toBe(24);
    if (edited) panel.edit(36);
    resolve(submitted);
    await saving;
    TestBed.tick();
    expect(panel.value()).toBe(edited ? 36 : 24);
    expect(panel.isDirty()).toBe(edited);
    expect(panel.isSaved()).toBe(!edited);
    expect(refreshStatus).toHaveBeenCalledTimes(1);
    if (edited) {
      const retry = panel.submit();
      resolve(submitted);
      await retry;
      TestBed.tick();
      expect(panel.value()).toBe(36);
      expect(panel.isDirty()).toBe(false);
      expect(panel.isSaved()).toBe(true);
    }
  });

  it('keeps edits on failure and permits retry', async () => {
    const saving = panel.submit();
    panel.edit(36);
    reject(new Error('Cannot persist settings'));
    await saving;
    TestBed.tick();
    expect(panel.value()).toBe(36);
    expect(panel.isDirty()).toBe(true);
    expect(panel.isSaved()).toBe(false);
    expect(refreshStatus).not.toHaveBeenCalled();
    const retry = panel.submit();
    resolve(submitted);
    await retry;
    TestBed.tick();
    expect(panel.isDirty()).toBe(false);
    expect(panel.isSaved()).toBe(true);
  });

  if (kind === 'tools') {
    it('uses confirmed folders when the policy draft still has an older folder list', async () => {
      const settingsStore = TestBed.inject(SettingsStore);
      settingsStore.settings.set({ ...SETTINGS, tools: { ...SETTINGS.tools, workspaceFolders: ['/new'] } });
      TestBed.tick();
      const saving = panel.submit();
      expect(submitted.tools.workspaceFolders).toEqual(['/new']);
      resolve(submitted);
      await saving;
      TestBed.tick();
      expect(settingsStore.settings()?.tools.workspaceFolders).toEqual(['/new']);
      expect(panel.isDirty()).toBe(false);
    });

    it('normalizes submitted programs without replacing newer program edits', async () => {
      const tools = panel as ToolsHarness;
      tools.programs('git, git, cargo');
      const saving = tools.submit();
      expect(submitted.tools.allowedPrograms).toEqual(['git', 'cargo']);
      tools.programs('git, rustc');
      resolve(submitted);
      await saving;
      TestBed.tick();
      expect(tools.programValue()).toBe('git, rustc');
      expect(tools.isDirty()).toBe(true);
      const retry = tools.submit();
      resolve(submitted);
      await retry;
      TestBed.tick();
      expect(tools.programValue()).toBe('git, rustc');
      expect(tools.isDirty()).toBe(false);
    });
  }
});

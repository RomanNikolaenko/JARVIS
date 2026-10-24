import { ChangeDetectionStrategy, Component, computed, effect, inject, signal, untracked } from '@angular/core';
import { FormsModule } from '@angular/forms';
import type { AgentsSettings, RiskLevel, Settings, ToolInfo, ToolSettings } from '@jarvis/shared-types';

import { AgentsStore } from '../../../core/state/agents.store';
import { SettingsStore } from '../../../core/state/settings.store';
import { Icon } from '../../../shared/icon/icon';

interface Draft {
  tools: ToolSettings;
  agents: AgentsSettings;
  allowCloudAgents: boolean;
  programs: string;
}

const RISK_GROUPS: readonly { risk: RiskLevel; title: string; hint: string }[] = [
  {
    risk: 'low',
    title: 'Read-only',
    hint: 'Read files, list folders and processes, read git history.',
  },
  {
    risk: 'medium',
    title: 'Changes',
    hint: 'Create and edit files, copy and move, load or open web pages.',
  },
  {
    risk: 'high',
    title: 'High risk',
    hint: 'Delete files, run and stop programs. Always asks for approval.',
  },
];

function draftOf(settings: Settings): Draft {
  return {
    tools: structuredClone(settings.tools),
    agents: { ...settings.agents },
    allowCloudAgents: settings.privacy.allowCloudAgents,
    programs: settings.tools.allowedPrograms.join(', '),
  };
}

function parsePrograms(text: string): string[] {
  const names = text
    .split(/[,\n]/)
    .map((p) => p.trim())
    .filter((p) => p.length > 0);
  return [...new Set(names)];
}

/** Settings of the Security Policy Engine: folders, tools, approvals, limits. */
@Component({
  selector: 'jv-tools-panel',
  imports: [FormsModule, Icon],
  templateUrl: './tools-panel.html',
  styleUrls: ['../agents-shared.scss', './tools-panel.scss'],
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class ToolsPanel {
  protected readonly store = inject(AgentsStore);
  private readonly settingsStore = inject(SettingsStore);

  protected readonly groups = RISK_GROUPS;
  protected readonly saving = this.settingsStore.saving;
  protected readonly saveError = this.settingsStore.error;
  protected readonly draft = signal<Draft | null>(null);
  protected readonly savedAt = signal<number | null>(null);
  protected readonly allowCloudProviders = computed(() => this.settingsStore.settings()?.privacy.allowCloudProviders ?? false);
  protected readonly dirty = computed(() => {
    const settings = this.settingsStore.settings();
    const draft = this.draft();
    return !!settings && !!draft && JSON.stringify(draftOf(settings)) !== JSON.stringify(draft);
  });

  constructor() {
    // Mirror stored settings unless the user is editing.
    effect(() => {
      const settings = this.settingsStore.settings();
      if (settings && !untracked(() => this.dirty())) {
        this.draft.set(draftOf(settings));
      }
    });
  }

  protected toolsOf(risk: RiskLevel): ToolInfo[] {
    return this.store.tools().filter((t) => t.spec.risk === risk);
  }

  protected isEnabled(name: string): boolean {
    return this.draft()?.tools.enabled.includes(name) ?? false;
  }

  protected toggleTool(name: string, on: boolean): void {
    this.update((d) => {
      const enabled = d.tools.enabled.filter((n) => n !== name);
      d.tools.enabled = on ? [...enabled, name] : enabled;
    });
  }

  protected setTools<K extends keyof ToolSettings>(key: K, value: ToolSettings[K]): void {
    this.update((d) => (d.tools[key] = value));
  }

  protected setAgents<K extends keyof AgentsSettings>(key: K, value: AgentsSettings[K]): void {
    this.update((d) => (d.agents[key] = value));
  }

  protected setCloudAgents(value: boolean): void {
    this.update((d) => (d.allowCloudAgents = value));
  }

  protected setPrograms(value: string): void {
    this.update((d) => (d.programs = value));
  }

  private update(change: (draft: Draft) => void): void {
    const current = this.draft();
    if (!current) {
      return;
    }
    const next = structuredClone(current);
    change(next);
    this.draft.set(next);
    this.savedAt.set(null);
  }

  protected folderAvailable(path: string): boolean {
    return this.store.status()?.workspaceFolders.find((f) => f.path === path)?.available ?? true;
  }

  /** Folders change right away (the dialog runs in the core). */
  protected async addFolder(): Promise<void> {
    if (this.saving()) return;
    if (!(await this.store.addWorkspaceFolder())) return;
    // The folder is saved at once; other unsaved edits in the form stay.
    const folders = this.settingsStore.settings()?.tools.workspaceFolders;
    if (folders) {
      this.update((d) => (d.tools.workspaceFolders = [...folders]));
    }
  }

  protected async removeFolder(path: string): Promise<void> {
    if (this.saving()) return;
    const settings = this.settingsStore.settings();
    if (!settings) {
      return;
    }
    const workspaceFolders = settings.tools.workspaceFolders.filter((f) => f !== path);
    if (await this.settingsStore.save({ ...settings, tools: { ...settings.tools, workspaceFolders } })) {
      this.update((d) => (d.tools.workspaceFolders = workspaceFolders));
      await this.store.refreshStatus();
    }
  }

  protected async save(): Promise<void> {
    if (this.saving()) return;
    const settings = this.settingsStore.settings();
    const draft = this.draft();
    if (!settings || !draft) {
      return;
    }
    const next: Settings = {
      ...settings,
      tools: {
        ...draft.tools,
        // Folder actions persist immediately; the draft may still await a status refresh.
        workspaceFolders: settings.tools.workspaceFolders,
        allowedPrograms: parsePrograms(draft.programs),
      },
      agents: draft.agents,
      privacy: { ...settings.privacy, allowCloudAgents: draft.allowCloudAgents },
    };
    this.savedAt.set(null);
    if (await this.settingsStore.save(next)) {
      if (JSON.stringify(this.draft()) === JSON.stringify(draft)) {
        this.draft.set(draftOf(this.settingsStore.settings() ?? next));
        this.savedAt.set(Date.now());
      }
      await Promise.all([this.store.loadTools(), this.store.refreshStatus()]);
    }
  }

  protected discard(): void {
    if (this.saving()) return;
    const settings = this.settingsStore.settings();
    if (settings) {
      this.draft.set(draftOf(settings));
    }
  }
}

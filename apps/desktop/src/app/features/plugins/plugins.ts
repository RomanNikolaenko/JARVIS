import { ChangeDetectionStrategy, Component, inject, signal } from '@angular/core';
import { FormControl, FormGroup, ReactiveFormsModule, Validators } from '@angular/forms';
import type { McpServerRequest, PluginInfo, PluginStatus, PluginToolInfo, RiskLevel, ToolHints } from '@jarvis/shared-types';

import { PluginsStore } from '../../core/state/plugins.store';
import { HudPanel } from '../../shared/hud-panel/hud-panel';
import { Icon } from '../../shared/icon/icon';

/** Parses the MCP server form into a request, or returns what is wrong. */
export function serverRequest(form: {
  name: string;
  program: string;
  args: string;
  cwd: string;
  env: string;
  secrets: string;
}): McpServerRequest | string {
  const env = [];
  for (const line of form.env.split('\n').map((l) => l.trim())) {
    if (!line) {
      continue;
    }
    const split = line.indexOf('=');
    if (split <= 0) {
      return `"${line}" is not NAME=value`;
    }
    env.push({ name: line.slice(0, split).trim(), value: line.slice(split + 1) });
  }
  return {
    name: form.name.trim(),
    program: form.program.trim(),
    args: form.args
      .split('\n')
      .map((a) => a.trim())
      .filter((a) => a.length > 0),
    cwd: form.cwd.trim() || null,
    env,
    secretNames: form.secrets.split(/[\s,]+/).filter((s) => s.length > 0),
  };
}

export function statusLabel(status: PluginStatus): string {
  switch (status.state) {
    case 'disabled':
      return 'Disabled';
    case 'needsApproval':
      return 'Needs approval';
    case 'needsSecrets':
      return 'Needs secrets';
    case 'starting':
      return 'Starting…';
    case 'running':
      return 'Running';
    case 'failed':
      return status.retryInSecs === null ? 'Stopped' : `Restarting in ${status.retryInSecs} s`;
  }
}

/** CSS class of a status: `status-needs-approval`. */
export function statusClass(status: PluginStatus): string {
  return `status-${status.state.replace(/[A-Z]/g, (c) => `-${c.toLowerCase()}`)}`;
}

/** What the server claims about a tool (untrusted, shown as hints). */
export function hintLabels(hints: ToolHints): string[] {
  const labels: string[] = [];
  if (hints.readOnly) {
    labels.push('read-only');
  }
  if (hints.destructive) {
    labels.push('destructive');
  }
  if (hints.openWorld) {
    labels.push('uses the internet');
  }
  return labels;
}

@Component({
  selector: 'jv-plugins',
  imports: [ReactiveFormsModule, HudPanel, Icon],
  templateUrl: './plugins.html',
  styleUrl: './plugins.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class PluginsPage {
  protected readonly store = inject(PluginsStore);

  protected readonly statusLabel = statusLabel;
  protected readonly statusClass = statusClass;
  protected readonly hintLabels = hintLabels;
  protected readonly risks: readonly RiskLevel[] = ['high', 'medium', 'low'];

  protected readonly showServerForm = signal(false);
  protected readonly formError = signal<string | null>(null);
  protected readonly confirmingRemoval = signal<string | null>(null);
  protected readonly openLogs = signal<ReadonlySet<string>>(new Set());
  /** Secret values being typed, by `<plugin>:<name>`; never shown again once saved. */
  protected readonly secretDrafts = signal<Readonly<Record<string, string>>>({});

  protected readonly serverForm = new FormGroup({
    name: new FormControl('', { nonNullable: true, validators: [Validators.required, Validators.maxLength(64)] }),
    program: new FormControl('', { nonNullable: true, validators: [Validators.required] }),
    args: new FormControl('', { nonNullable: true }),
    cwd: new FormControl('', { nonNullable: true }),
    env: new FormControl('', { nonNullable: true }),
    secrets: new FormControl('', { nonNullable: true }),
  });

  constructor() {
    void this.store.start();
  }

  protected async reviewServer(): Promise<void> {
    if (this.serverForm.invalid) {
      this.serverForm.markAllAsTouched();
      return;
    }
    const request = serverRequest(this.serverForm.getRawValue());
    if (typeof request === 'string') {
      this.formError.set(request);
      return;
    }
    this.formError.set(null);
    if (await this.store.reviewServer(request)) {
      this.showServerForm.set(false);
    }
  }

  protected async confirm(): Promise<void> {
    if (await this.store.confirmReview()) {
      this.serverForm.reset();
    }
  }

  protected toggleTool(plugin: PluginInfo, tool: PluginToolInfo, enabled: boolean): void {
    void this.store.setTool(plugin.id, tool.name, enabled, tool.risk);
  }

  protected setRisk(plugin: PluginInfo, tool: PluginToolInfo, risk: string): void {
    void this.store.setTool(plugin.id, tool.name, tool.enabled, risk as RiskLevel);
  }

  protected secretKey(plugin: PluginInfo, name: string): string {
    return `${plugin.id}:${name}`;
  }

  protected typeSecret(key: string, value: string): void {
    this.secretDrafts.update((drafts) => ({ ...drafts, [key]: value }));
  }

  protected async saveSecret(plugin: PluginInfo, name: string): Promise<void> {
    const key = this.secretKey(plugin, name);
    const value = this.secretDrafts()[key] ?? '';
    if (!value) {
      return;
    }
    if (await this.store.setSecret(plugin.id, name, value)) {
      this.typeSecret(key, '');
    }
  }

  protected async remove(id: string): Promise<void> {
    if (await this.store.remove(id)) {
      this.confirmingRemoval.set(null);
    }
  }

  protected toggleLogs(id: string, open: boolean): void {
    this.openLogs.update((ids) => {
      const next = new Set(ids);
      if (open) {
        next.add(id);
      } else {
        next.delete(id);
      }
      return next;
    });
    if (open) {
      void this.store.loadLogs(id);
    }
  }

  protected shortFingerprint(fingerprint: string | null): string {
    return fingerprint ? fingerprint.slice(0, 16) : '—';
  }
}

import { Injectable, inject, signal } from '@angular/core';
import type { McpServerRequest, PluginInfo, PluginReview, RiskLevel } from '@jarvis/shared-types';

import { IpcError } from '../ipc/ipc-error';
import { JarvisApi } from '../ipc/jarvis-api';

/**
 * Plugins (MCP servers). The core pushes the whole list after every change;
 * adding a plugin always goes through a review the user confirms.
 */
@Injectable({ providedIn: 'root' })
export class PluginsStore {
  private readonly api = inject(JarvisApi);
  private started = false;

  readonly plugins = signal<PluginInfo[]>([]);
  readonly loaded = signal(false);
  readonly error = signal<IpcError | null>(null);
  /** The review waiting for the user's decision. */
  readonly review = signal<PluginReview | null>(null);
  /** Keys of commands in progress (`plugin:<id>`, `review`, `install`). */
  readonly pending = signal<ReadonlySet<string>>(new Set());
  /** Recent server output by plugin id, loaded on request. */
  readonly logs = signal<Readonly<Record<string, string[]>>>({});

  async start(): Promise<void> {
    if (this.started) {
      return;
    }
    this.started = true;
    try {
      await this.api.onPlugins((plugins) => this.plugins.set(plugins));
      this.plugins.set(await this.api.listPlugins());
      this.error.set(null);
    } catch (error) {
      this.started = false;
      this.error.set(IpcError.from(error));
    } finally {
      this.loaded.set(true);
    }
  }

  isPending(key: string): boolean {
    return this.pending().has(key);
  }

  /** Picks a plugin folder; its review is shown when the user chose one. */
  pickFolder(): Promise<boolean> {
    return this.command('review', async () => {
      const review = await this.api.pickPluginFolder();
      if (review) {
        this.review.set(review);
      }
    });
  }

  reviewServer(request: McpServerRequest): Promise<boolean> {
    return this.command('review', async () => this.review.set(await this.api.reviewMcpServer(request)));
  }

  reviewInstalled(id: string): Promise<boolean> {
    return this.command(`plugin:${id}`, async () => this.review.set(await this.api.reviewInstalledPlugin(id)));
  }

  /** Saves and starts the reviewed plugin. */
  async confirmReview(): Promise<boolean> {
    const review = this.review();
    if (!review) {
      return false;
    }
    const done = await this.command('install', async () => this.upsert(await this.api.installPlugin(review.reviewId)));
    if (done) {
      this.review.set(null);
    }
    return done;
  }

  cancelReview(): void {
    this.review.set(null);
  }

  setEnabled(id: string, enabled: boolean): Promise<boolean> {
    return this.pluginCommand(id, () => this.api.setPluginEnabled(id, enabled));
  }

  restart(id: string): Promise<boolean> {
    return this.pluginCommand(id, () => this.api.restartPlugin(id));
  }

  refreshTools(id: string): Promise<boolean> {
    return this.pluginCommand(id, () => this.api.refreshPluginTools(id));
  }

  setTool(pluginId: string, tool: string, enabled: boolean, risk: RiskLevel): Promise<boolean> {
    return this.pluginCommand(pluginId, () => this.api.setPluginTool({ pluginId, tool, enabled, risk }));
  }

  setSecret(id: string, name: string, value: string): Promise<boolean> {
    return this.pluginCommand(id, () => this.api.setPluginSecret(id, name, value));
  }

  clearSecret(id: string, name: string): Promise<boolean> {
    return this.pluginCommand(id, () => this.api.clearPluginSecret(id, name));
  }

  remove(id: string): Promise<boolean> {
    return this.command(`plugin:${id}`, async () => {
      await this.api.removePlugin(id);
      this.plugins.update((plugins) => plugins.filter((p) => p.id !== id));
    });
  }

  loadLogs(id: string): Promise<boolean> {
    return this.command(`logs:${id}`, async () => {
      const lines = await this.api.getPluginLogs(id);
      this.logs.update((logs) => ({ ...logs, [id]: lines }));
    });
  }

  private upsert(plugin: PluginInfo): void {
    this.plugins.update((plugins) =>
      plugins.some((p) => p.id === plugin.id) ? plugins.map((p) => (p.id === plugin.id ? plugin : p)) : [...plugins, plugin]
    );
  }

  private pluginCommand(id: string, action: () => Promise<PluginInfo>): Promise<boolean> {
    return this.command(`plugin:${id}`, async () => this.upsert(await action()));
  }

  private async command(key: string, action: () => Promise<void>): Promise<boolean> {
    if (this.isPending(key)) {
      return false;
    }
    this.pending.update((keys) => new Set(keys).add(key));
    try {
      await action();
      this.error.set(null);
      return true;
    } catch (error) {
      this.error.set(IpcError.from(error));
      return false;
    } finally {
      this.pending.update((keys) => {
        const next = new Set(keys);
        next.delete(key);
        return next;
      });
    }
  }
}

import { Injectable, inject } from '@angular/core';
import type { UnlistenFn } from '@tauri-apps/api/event';
import type {
  AppInfo,
  ChatStartRequest,
  ChatStreamEvent,
  HealthReport,
  PlatformInfo,
  ProviderModels,
  Settings,
  SystemEvent,
  SystemMetrics,
  UiErrorReport,
} from '@jarvis/shared-types';

import { TauriBridge } from './tauri-bridge';

/** Name of the event used by the core to push system events. */
export const SYSTEM_EVENT = 'jarvis://system-event';

/**
 * Typed facade over the IPC commands registered in
 * `src-tauri/src/lib.rs`. Command and argument names must match the Rust side.
 */
@Injectable({ providedIn: 'root' })
export class JarvisApi {
  private readonly bridge = inject(TauriBridge);

  get connected(): boolean {
    return this.bridge.available;
  }

  getAppInfo(): Promise<AppInfo> {
    return this.bridge.invoke('get_app_info');
  }

  getHealth(): Promise<HealthReport> {
    return this.bridge.invoke('get_health');
  }

  getPlatformInfo(): Promise<PlatformInfo> {
    return this.bridge.invoke('get_platform_info');
  }

  getSystemMetrics(): Promise<SystemMetrics> {
    return this.bridge.invoke('get_system_metrics');
  }

  getSettings(): Promise<Settings> {
    return this.bridge.invoke('get_settings');
  }

  updateSettings(settings: Settings): Promise<Settings> {
    return this.bridge.invoke('update_settings', { settings });
  }

  getRecentEvents(limit?: number): Promise<SystemEvent[]> {
    return this.bridge.invoke('get_recent_events', { limit: limit ?? null });
  }

  listAiModels(): Promise<ProviderModels[]> {
    return this.bridge.invoke('list_ai_models');
  }

  /**
   * Starts a streamed chat. Events may arrive before the returned promise
   * resolves; the `started` event carries the request id as well.
   */
  startChat(request: ChatStartRequest, onEvent: (event: ChatStreamEvent) => void): Promise<string> {
    const channel = this.bridge.channel<ChatStreamEvent>(onEvent);
    return this.bridge.invoke('start_chat', { request, onEvent: channel });
  }

  cancelChat(requestId: string): Promise<boolean> {
    return this.bridge.invoke('cancel_chat', { requestId });
  }

  reportUiError(report: UiErrorReport): Promise<void> {
    return this.bridge.invoke('report_ui_error', { report });
  }

  onSystemEvent(handler: (event: SystemEvent) => void): Promise<UnlistenFn> {
    return this.bridge.listen<SystemEvent>(SYSTEM_EVENT, handler);
  }
}

import { Injectable, inject } from '@angular/core';
import type { UnlistenFn } from '@tauri-apps/api/event';
import type {
  AppInfo,
  ChatStartRequest,
  CuratedModel,
  HardwareInfo,
  LocalModel,
  ModelDownloadRequest,
  RuntimeBackend,
  RuntimeBinary,
  RuntimeRelease,
  RuntimeStatus,
  ChatStreamEvent,
  HealthReport,
  PlatformInfo,
  ProviderConfig,
  ProviderModels,
  ProviderTestResult,
  RouteResolution,
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

  /** Models of all providers; cached briefly by the core unless `refresh` is set. */
  listAiModels(refresh = false): Promise<ProviderModels[]> {
    return this.bridge.invoke('list_ai_models', { refresh });
  }

  getModelRoutes(): Promise<RouteResolution[]> {
    return this.bridge.invoke('get_model_routes');
  }

  /** Connects to an unsaved provider configuration and lists its models. */
  testAiProvider(config: ProviderConfig): Promise<ProviderTestResult> {
    return this.bridge.invoke('test_ai_provider', { config });
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

  // ---- Local runtime (llama.cpp) ----

  getRuntimeStatus(): Promise<RuntimeStatus> {
    return this.bridge.invoke('get_runtime_status');
  }

  getHardwareInfo(): Promise<HardwareInfo> {
    return this.bridge.invoke('get_hardware_info');
  }

  listCuratedModels(): Promise<CuratedModel[]> {
    return this.bridge.invoke('list_curated_models');
  }

  /** Looks up the newest official llama.cpp build on GitHub. */
  checkRuntimeRelease(backend: RuntimeBackend): Promise<RuntimeRelease> {
    return this.bridge.invoke('check_runtime_release', { backend });
  }

  /** Installs a release the user approved; returns the download id. */
  installRuntime(release: RuntimeRelease): Promise<string> {
    return this.bridge.invoke('install_runtime', { release });
  }

  /** Opens a native file picker in the core; `null` if cancelled. */
  selectRuntimeBinary(): Promise<RuntimeBinary | null> {
    return this.bridge.invoke('select_runtime_binary');
  }

  /** Opens a native file picker in the core; `null` if cancelled. */
  importModel(): Promise<LocalModel | null> {
    return this.bridge.invoke('import_model');
  }

  downloadCuratedModel(id: string): Promise<string> {
    return this.bridge.invoke('download_curated_model', { id });
  }

  downloadModel(request: ModelDownloadRequest): Promise<string> {
    return this.bridge.invoke('download_model', { request });
  }

  cancelDownload(id: string): Promise<boolean> {
    return this.bridge.invoke('cancel_download', { id });
  }

  removeModel(id: string, deleteFile: boolean): Promise<void> {
    return this.bridge.invoke('remove_model', { id, deleteFile });
  }

  startLocalModel(modelId: string): Promise<void> {
    return this.bridge.invoke('start_local_model', { modelId });
  }

  stopLocalModel(): Promise<void> {
    return this.bridge.invoke('stop_local_model');
  }

  reportUiError(report: UiErrorReport): Promise<void> {
    return this.bridge.invoke('report_ui_error', { report });
  }

  onSystemEvent(handler: (event: SystemEvent) => void): Promise<UnlistenFn> {
    return this.bridge.listen<SystemEvent>(SYSTEM_EVENT, handler);
  }
}

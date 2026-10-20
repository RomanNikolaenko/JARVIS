import { Injectable, inject } from '@angular/core';
import type { UnlistenFn } from '@tauri-apps/api/event';
import type {
  AppInfo,
  ChatTurnRequest,
  Conversation,
  ConversationDetail,
  ConversationPatch,
  CuratedModel,
  Episode,
  Fact,
  FactInput,
  MemoryStatus,
  Project,
  ProjectInput,
  SearchHit,
  SearchQuery,
  TransferReport,
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
   * Sends a message in a conversation; the core builds the context and saves
   * both sides. Events may arrive before the returned promise resolves; the
   * `started` event carries the request id as well.
   */
  sendMessage(
    request: ChatTurnRequest,
    onEvent: (event: ChatStreamEvent) => void,
  ): Promise<string> {
    const channel = this.bridge.channel<ChatStreamEvent>(onEvent);
    return this.bridge.invoke('send_message', { request, onEvent: channel });
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

  // ---- Memory ----

  listConversations(query?: string): Promise<Conversation[]> {
    return this.bridge.invoke('list_conversations', { query: query ?? null, limit: null });
  }

  getConversation(id: string): Promise<ConversationDetail> {
    return this.bridge.invoke('get_conversation', { id });
  }

  createConversation(projectId: string | null, temporary: boolean): Promise<Conversation> {
    return this.bridge.invoke('create_conversation', { projectId, temporary });
  }

  updateConversation(id: string, patch: ConversationPatch): Promise<Conversation> {
    return this.bridge.invoke('update_conversation', { id, patch });
  }

  deleteConversation(id: string): Promise<void> {
    return this.bridge.invoke('delete_conversation', { id });
  }

  listFacts(): Promise<Fact[]> {
    return this.bridge.invoke('list_facts');
  }

  saveFact(input: FactInput): Promise<Fact> {
    return this.bridge.invoke('save_fact', { input });
  }

  deleteFact(id: string): Promise<void> {
    return this.bridge.invoke('delete_fact', { id });
  }

  listProjects(): Promise<Project[]> {
    return this.bridge.invoke('list_projects');
  }

  saveProject(input: ProjectInput): Promise<Project> {
    return this.bridge.invoke('save_project', { input });
  }

  deleteProject(id: string): Promise<void> {
    return this.bridge.invoke('delete_project', { id });
  }

  listEpisodes(limit: number, beforeMs: number | null): Promise<Episode[]> {
    return this.bridge.invoke('list_episodes', { limit, beforeMs });
  }

  clearEpisodes(): Promise<number> {
    return this.bridge.invoke('clear_episodes');
  }

  searchMemory(query: SearchQuery): Promise<SearchHit[]> {
    return this.bridge.invoke('search_memory', { query });
  }

  getMemoryStatus(): Promise<MemoryStatus> {
    return this.bridge.invoke('get_memory_status');
  }

  /** Opens a save dialog in the core; `null` when the user cancelled. */
  exportMemory(): Promise<TransferReport | null> {
    return this.bridge.invoke('export_memory');
  }

  /** Opens a file dialog in the core; `null` when the user cancelled. */
  importMemory(): Promise<TransferReport | null> {
    return this.bridge.invoke('import_memory');
  }

  /** Deletes all saved memory; `confirmation` must be the agreed phrase. */
  clearMemory(confirmation: string): Promise<void> {
    return this.bridge.invoke('clear_memory', { confirmation });
  }

  reportUiError(report: UiErrorReport): Promise<void> {
    return this.bridge.invoke('report_ui_error', { report });
  }

  onSystemEvent(handler: (event: SystemEvent) => void): Promise<UnlistenFn> {
    return this.bridge.listen<SystemEvent>(SYSTEM_EVENT, handler);
  }
}

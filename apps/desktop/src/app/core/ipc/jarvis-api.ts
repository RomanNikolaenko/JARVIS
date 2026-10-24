import { Injectable, inject } from '@angular/core';
import type { UnlistenFn } from '@tauri-apps/api/event';
import type {
  AgentStartRequest,
  AgentStatus,
  AgentTask,
  AuditEntry,
  AuditVerification,
  PendingApproval,
  TaskSummary,
  ToolInfo,
  AppInfo,
  ChatTurnRequest,
  Conversation,
  ConversationDetail,
  ConversationPatch,
  CuratedModel,
  Episode,
  EpisodeCursor,
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
  ServerRole,
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
  CaptureInfo,
  VisionStatus,
  WindowInfo,
  VoiceState,
  VoiceStatus,
  VoiceTranscript,
} from '@jarvis/shared-types';

import { TauriBridge } from './tauri-bridge';

/** Name of the event used by the core to push system events. */
export const SYSTEM_EVENT = 'jarvis://system-event';
/** Pushed with the whole task whenever an agent task changes. */
export const AGENT_TASK_EVENT = 'jarvis://agent-task';
/** Pushed with every pending approval whenever the list changes. */
export const APPROVALS_EVENT = 'jarvis://approvals';
/** Pushed with the voice engine's state (microphone, listening, speaking). */
export const VOICE_STATE_EVENT = 'jarvis://voice-state';
/** Pushed with every recognized spoken request. */
export const VOICE_TRANSCRIPT_EVENT = 'jarvis://voice-transcript';
/** Pushed with the microphone level (0–1) while it is open. */
export const VOICE_LEVEL_EVENT = 'jarvis://voice-level';

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
  testAiProvider(config: ProviderConfig, apiKey?: string): Promise<ProviderTestResult> {
    return this.bridge.invoke('test_ai_provider', { config, apiKey });
  }

  setProviderApiKey(providerId: string, apiKey: string): Promise<void> {
    return this.bridge.invoke('set_provider_api_key', { providerId, apiKey });
  }

  hasProviderApiKey(providerId: string): Promise<boolean> {
    return this.bridge.invoke('has_provider_api_key', { providerId });
  }

  deleteProviderApiKey(providerId: string): Promise<void> {
    return this.bridge.invoke('delete_provider_api_key', { providerId });
  }

  /**
   * Sends a message in a conversation; the core builds the context and saves
   * both sides. Events may arrive before the returned promise resolves; the
   * `started` event carries the request id as well.
   */
  sendMessage(request: ChatTurnRequest, onEvent: (event: ChatStreamEvent) => void): Promise<string> {
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

  /** Starts a model in the chat or the embedding server. */
  startLocalModel(modelId: string, role: ServerRole): Promise<void> {
    return this.bridge.invoke('start_local_model', { modelId, role });
  }

  stopLocalModel(role: ServerRole): Promise<void> {
    return this.bridge.invoke('stop_local_model', { role });
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

  listEpisodes(limit: number, before: EpisodeCursor | null): Promise<Episode[]> {
    return this.bridge.invoke('list_episodes', { limit, before });
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

  // ---- Agents and tools ----

  getAgentStatus(): Promise<AgentStatus> {
    return this.bridge.invoke('get_agent_status');
  }

  listTools(): Promise<ToolInfo[]> {
    return this.bridge.invoke('list_tools');
  }

  startAgentTask(request: AgentStartRequest): Promise<AgentTask> {
    return this.bridge.invoke('start_agent_task', { request });
  }

  listAgentTasks(limit?: number): Promise<TaskSummary[]> {
    return this.bridge.invoke('list_agent_tasks', { limit: limit ?? null });
  }

  getAgentTask(id: string): Promise<AgentTask> {
    return this.bridge.invoke('get_agent_task', { id });
  }

  cancelAgentTask(id: string): Promise<boolean> {
    return this.bridge.invoke('cancel_agent_task', { id });
  }

  pauseAgentTask(id: string): Promise<boolean> {
    return this.bridge.invoke('pause_agent_task', { id });
  }

  resumeAgentTask(id: string): Promise<void> {
    return this.bridge.invoke('resume_agent_task', { id });
  }

  clearFinishedAgentTasks(): Promise<number> {
    return this.bridge.invoke('clear_finished_agent_tasks');
  }

  listPendingApprovals(): Promise<PendingApproval[]> {
    return this.bridge.invoke('list_pending_approvals');
  }

  /** `false` when the action is no longer waiting (decided, timed out, cancelled). */
  resolveApproval(id: string, approved: boolean): Promise<boolean> {
    return this.bridge.invoke('resolve_approval', { id, approved });
  }

  getAuditLog(limit: number, beforeId: string | null): Promise<AuditEntry[]> {
    return this.bridge.invoke('get_audit_log', { limit, beforeId });
  }

  verifyAuditLog(): Promise<AuditVerification> {
    return this.bridge.invoke('verify_audit_log');
  }

  /** Opens a folder dialog in the core; `null` when the user cancelled. */
  addWorkspaceFolder(): Promise<Settings | null> {
    return this.bridge.invoke('add_workspace_folder');
  }

  onAgentTask(handler: (task: AgentTask) => void): Promise<UnlistenFn> {
    return this.bridge.listen<AgentTask>(AGENT_TASK_EVENT, handler);
  }

  onApprovals(handler: (approvals: PendingApproval[]) => void): Promise<UnlistenFn> {
    return this.bridge.listen<PendingApproval[]>(APPROVALS_EVENT, handler);
  }

  reportUiError(report: UiErrorReport): Promise<void> {
    return this.bridge.invoke('report_ui_error', { report });
  }

  onSystemEvent(handler: (event: SystemEvent) => void): Promise<UnlistenFn> {
    return this.bridge.listen<SystemEvent>(SYSTEM_EVENT, handler);
  }

  // ---- Vision ----

  getVisionStatus(): Promise<VisionStatus> {
    return this.bridge.invoke('get_vision_status');
  }

  listWindows(): Promise<WindowInfo[]> {
    return this.bridge.invoke('list_windows');
  }

  /** Captures a display (`null`: the primary one); `hideJarvis` steps the window aside. */
  captureScreen(display: string | null, hideJarvis: boolean): Promise<CaptureInfo> {
    return this.bridge.invoke('capture_screen', { display, hideJarvis });
  }

  captureWindow(id: string): Promise<CaptureInfo> {
    return this.bridge.invoke('capture_window', { id });
  }

  /** A window or area picked in the system dialog (Wayland). */
  captureSelection(): Promise<CaptureInfo> {
    return this.bridge.invoke('capture_selection');
  }

  /** Opens a file dialog in the core; `null` when the user cancelled. */
  attachImageFile(): Promise<CaptureInfo | null> {
    return this.bridge.invoke('attach_image_file');
  }

  /** A camera photo as a `data:` URL. */
  attachPhoto(data: string): Promise<CaptureInfo> {
    return this.bridge.invoke('attach_photo', { data });
  }

  discardCapture(id: string): Promise<boolean> {
    return this.bridge.invoke('discard_capture', { id });
  }

  /** Opens a file dialog in the core to add a vision projector to a model. */
  attachProjector(modelId: string): Promise<LocalModel | null> {
    return this.bridge.invoke('attach_projector', { modelId });
  }

  // ---- Voice ----

  getVoiceStatus(): Promise<VoiceStatus> {
    return this.bridge.invoke('get_voice_status');
  }

  /** Installs a voice package the user approved; returns the download id. */
  installVoicePackage(id: string): Promise<string> {
    return this.bridge.invoke('install_voice_package', { id });
  }

  removeVoicePackage(id: string): Promise<void> {
    return this.bridge.invoke('remove_voice_package', { id });
  }

  cancelVoiceDownload(id: string): Promise<boolean> {
    return this.bridge.invoke('cancel_voice_download', { id });
  }

  /** Opens the microphone for one spoken request. */
  startListening(): Promise<void> {
    return this.bridge.invoke('start_listening');
  }

  /** `submit` recognizes what was said so far; otherwise it is dropped. */
  stopListening(submit: boolean): Promise<void> {
    return this.bridge.invoke('stop_listening', { submit });
  }

  stopSpeaking(): Promise<void> {
    return this.bridge.invoke('stop_speaking');
  }

  speakText(text: string): Promise<void> {
    return this.bridge.invoke('speak_text', { text });
  }

  onVoiceState(handler: (state: VoiceState) => void): Promise<UnlistenFn> {
    return this.bridge.listen<VoiceState>(VOICE_STATE_EVENT, handler);
  }

  onVoiceTranscript(handler: (transcript: VoiceTranscript) => void): Promise<UnlistenFn> {
    return this.bridge.listen<VoiceTranscript>(VOICE_TRANSCRIPT_EVENT, handler);
  }

  onVoiceLevel(handler: (level: number) => void): Promise<UnlistenFn> {
    return this.bridge.listen<number>(VOICE_LEVEL_EVENT, handler);
  }
}

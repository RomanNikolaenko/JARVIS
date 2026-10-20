import type { Settings } from '@jarvis/shared-types';

/** Valid settings for tests (defaults with one Ollama route). */
export const SETTINGS = {
  schemaVersion: 1,
  general: { assistantLanguage: 'uk' },
  interface: { reducedMotion: false },
  ai: {
    providers: [
      {
        id: 'ollama',
        backend: 'ollama',
        displayName: 'Ollama',
        baseUrl: 'http://127.0.0.1:11434',
        enabled: true,
      },
    ],
    routing: {
      general: { providerId: 'ollama', model: 'x' },
      reasoning: null,
      coding: null,
      vision: null,
      embeddings: null,
      toolCalling: null,
    },
    requestTimeoutSecs: 300,
    idleTimeoutSecs: 120,
  },
  runtime: {
    contextSize: 4096,
    threads: null,
    gpuOffload: 'auto',
    gpuLayers: 0,
    autostartModel: null,
    startupTimeoutSecs: 180,
  },
  memory: {
    enabled: true,
    useFacts: true,
    recallConversations: true,
    autoSummarize: true,
    embeddingModel: null,
    contextLimitTokens: null,
    episodeRetentionDays: 90,
  },
  tools: {
    enabled: [
      'list_directory',
      'read_file',
      'search_files',
      'system_info',
      'list_processes',
      'git',
    ],
    workspaceFolders: [],
    allowedPrograms: [],
    allowNetwork: false,
    autoApproveReadOnly: true,
    autoApproveChanges: false,
    commandTimeoutSecs: 60,
  },
  agents: { enabled: true, maxSteps: 12, maxDurationSecs: 600, approvalTimeoutSecs: 300 },
  privacy: { allowCloudProviders: false, shareMemoryWithCloud: false, allowCloudAgents: false },
  logging: { level: 'info' },
} satisfies Settings;

import type { PluginInfo, PluginReview, PluginToolInfo } from '@jarvis/shared-types';

export function pluginTool(name: string, overrides: Partial<PluginToolInfo> = {}): PluginToolInfo {
  return {
    name,
    id: `notes__${name}`,
    title: name,
    description: `${name} description`,
    enabled: true,
    risk: 'high',
    riskReset: false,
    hints: { readOnly: null, destructive: null, idempotent: null, openWorld: null },
    ...overrides,
  };
}

export function pluginInfo(overrides: Partial<PluginInfo> = {}): PluginInfo {
  return {
    id: 'notes',
    name: 'Notes',
    version: '1.0.0',
    description: 'Searches notes.',
    author: null,
    homepage: null,
    source: 'folder',
    folder: 'C:\\plugins\\notes',
    launch: {
      program: 'node',
      resolvedProgram: 'C:\\Program Files\\nodejs\\node.exe',
      args: ['server.mjs'],
      cwd: 'C:\\plugins\\notes',
      envNames: [],
    },
    enabled: true,
    status: { state: 'running' },
    server: { name: 'notes-server', version: '1.0.0', title: null, protocolVersion: '2026-07-28', instructions: null },
    tools: [pluginTool('search', { hints: { readOnly: true, destructive: null, idempotent: null, openWorld: null } })],
    toolProblems: [],
    secrets: [],
    warnings: [],
    fingerprint: 'a'.repeat(64),
    addedAtMs: 1,
    ...overrides,
  };
}

export function pluginReview(overrides: Partial<PluginReview> = {}): PluginReview {
  return {
    reviewId: 'review-1',
    source: 'command',
    id: 'my-server',
    name: 'My server',
    version: null,
    description: null,
    author: null,
    homepage: null,
    folder: null,
    launch: {
      program: 'npx',
      resolvedProgram: 'C:\\Program Files\\nodejs\\npx.cmd',
      args: ['-y', 'some-server'],
      cwd: 'C:\\data\\plugins\\my-server',
      envNames: ['MODE'],
    },
    secrets: [{ name: 'API_KEY', description: null }],
    fingerprint: 'b'.repeat(64),
    warnings: ['npx downloads and runs packages when the server starts; JARVIS cannot check what it runs.'],
    replaces: false,
    ...overrides,
  };
}

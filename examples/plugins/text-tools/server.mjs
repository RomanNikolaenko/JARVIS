#!/usr/bin/env node
// A minimal MCP server for JARVIS (and any other MCP client), without
// dependencies. It speaks the current protocol (2026-07-28: server/discover,
// version in every request's _meta) and the older handshake (initialize).
// Messages are newline-delimited JSON on stdin/stdout; logs go to stderr.

import { createInterface } from 'node:readline';

const MODERN = '2026-07-28';
const LEGACY = ['2025-11-25', '2025-06-18', '2025-03-26', '2024-11-05'];
const SERVER_INFO = { name: 'text-tools', version: '1.0.0', title: 'Text tools' };

const TOOLS = [
  {
    name: 'word_count',
    title: 'Count words',
    description: 'Counts words, characters and lines of a text.',
    inputSchema: {
      type: 'object',
      properties: { text: { type: 'string', description: 'The text to count.' } },
      required: ['text'],
      additionalProperties: false,
    },
    annotations: { readOnlyHint: true, openWorldHint: false },
  },
  {
    name: 'slugify',
    title: 'Make a slug',
    description: 'Turns a title into a lowercase, dash-separated slug for file names and URLs.',
    inputSchema: {
      type: 'object',
      properties: { text: { type: 'string' } },
      required: ['text'],
      additionalProperties: false,
    },
    annotations: { readOnlyHint: true, openWorldHint: false },
  },
];

function send(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

function result(id, value) {
  send({ jsonrpc: '2.0', id, result: { ...value, _meta: { 'io.modelcontextprotocol/serverInfo': SERVER_INFO } } });
}

function error(id, code, message, data) {
  send({ jsonrpc: '2.0', id, error: { code, message, ...(data === undefined ? {} : { data }) } });
}

function text(value, structured) {
  return {
    resultType: 'complete',
    content: [{ type: 'text', text: value }],
    ...(structured ? { structuredContent: structured } : {}),
  };
}

function callTool(name, args) {
  if (typeof args?.text !== 'string') {
    return { resultType: 'complete', content: [{ type: 'text', text: 'The input needs a "text" string.' }], isError: true };
  }
  switch (name) {
    case 'word_count': {
      const counts = {
        words: args.text.split(/\s+/u).filter((w) => w.length > 0).length,
        characters: [...args.text].length,
        lines: args.text.length === 0 ? 0 : args.text.split('\n').length,
      };
      return text(JSON.stringify(counts), counts);
    }
    case 'slugify': {
      const slug = args.text
        .normalize('NFKD')
        .replace(/\p{M}/gu, '')
        .toLowerCase()
        .replace(/[^\p{L}\p{N}]+/gu, '-')
        .replace(/^-+|-+$/g, '');
      return text(slug);
    }
    default:
      return null;
  }
}

function handle(message) {
  const { id, method, params = {} } = message;
  if (method === undefined || id === undefined || id === null) {
    // Notifications (initialized, cancelled) need no answer.
    return;
  }
  const version = params._meta?.['io.modelcontextprotocol/protocolVersion'];
  switch (method) {
    case 'server/discover':
      if (version !== MODERN) {
        error(id, -32022, 'Unsupported protocol version', { supported: [MODERN, ...LEGACY], requested: version ?? null });
        return;
      }
      result(id, {
        resultType: 'complete',
        supportedVersions: [MODERN],
        capabilities: { tools: {} },
        instructions: 'Text utilities: word_count and slugify.',
        ttlMs: 3_600_000,
        cacheScope: 'public',
      });
      return;
    case 'initialize': {
      const requested = params.protocolVersion;
      result(id, {
        protocolVersion: LEGACY.includes(requested) ? requested : LEGACY[0],
        capabilities: { tools: {} },
        serverInfo: SERVER_INFO,
      });
      return;
    }
    case 'ping':
      result(id, {});
      return;
    case 'tools/list':
      result(id, { resultType: 'complete', tools: TOOLS, ttlMs: 3_600_000, cacheScope: 'public' });
      return;
    case 'tools/call': {
      const answer = callTool(params.name, params.arguments);
      if (answer === null) {
        error(id, -32602, `Unknown tool: ${params.name}`);
      } else {
        result(id, answer);
      }
      return;
    }
    default:
      error(id, -32601, `Method not found: ${method}`);
  }
}

const input = createInterface({ input: process.stdin });
input.on('line', (line) => {
  if (!line.trim()) {
    return;
  }
  try {
    handle(JSON.parse(line));
  } catch (err) {
    console.error(`text-tools: cannot handle a message: ${err}`);
  }
});
input.on('close', () => process.exit(0));
console.error('text-tools: ready');

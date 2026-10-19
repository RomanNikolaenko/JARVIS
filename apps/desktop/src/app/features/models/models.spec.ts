import type { ModelRouting } from '@jarvis/shared-types';

import { providerIdFor, withoutProvider } from './models';

describe('Models page helpers', () => {
  it('derives unique, valid provider ids from names', () => {
    expect(providerIdFor('Ollama on GPU box', [])).toBe('ollama-on-gpu-box');
    expect(providerIdFor('Ollama', ['ollama'])).toBe('ollama-2');
    expect(providerIdFor('Ollama', ['ollama', 'ollama-2'])).toBe('ollama-3');
    expect(providerIdFor('Діагностика', [])).toBe('provider');
    expect(providerIdFor('Diagnostic Echo', [])).toBe('diagnostic-echo-2');
  });

  it('removes routes that point at a deleted provider', () => {
    const routing: ModelRouting = {
      general: { providerId: 'a', model: 'm1' },
      reasoning: null,
      coding: { providerId: 'b', model: 'm2' },
      vision: { providerId: 'a', model: 'm3' },
      embeddings: null,
      toolCalling: null,
    };
    expect(withoutProvider(routing, 'a')).toEqual({
      ...routing,
      general: null,
      vision: null,
    });
  });
});

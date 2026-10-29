import type { ModelRouting, Settings } from '@jarvis/shared-types';
import { TestBed } from '@angular/core/testing';
import { JarvisApi } from '../../core/ipc/jarvis-api';
import { SettingsStore } from '../../core/state/settings.store';
import { SETTINGS } from '../../testing/settings.fixture';

import { Models, providerIdFor, withoutProvider } from './models';

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

describe('Provider credentials form', () => {
  it('edits, persists and clears per-model native tool support', async () => {
    const api = {
      listAiModels: vi.fn(async () => []),
      getModelRoutes: vi.fn(async () => []),
      updateSettings: vi.fn(async (settings) => settings),
      hasProviderApiKey: vi.fn(async () => false),
    };
    TestBed.configureTestingModule({ providers: [{ provide: JarvisApi, useValue: api }] });
    const settings: Settings = structuredClone(SETTINGS);
    settings.ai.providers[0] = { ...settings.ai.providers[0], backend: 'openAiCompatible', nativeToolModels: ['old-model'] };
    const store = TestBed.inject(SettingsStore);
    store.settings.set(settings);
    const fixture = TestBed.createComponent(Models);
    fixture.detectChanges();
    await fixture.whenStable();
    const element = fixture.nativeElement as HTMLElement;
    const click = async (label: string) => {
      const button = [...element.querySelectorAll('button')].find((b) => b.textContent?.trim() === label)!;
      button.click();
      fixture.detectChanges();
      await fixture.whenStable();
      await vi.waitFor(() => {
        fixture.detectChanges();
        expect(element.querySelector('fieldset')?.disabled ?? false).toBe(false);
      });
    };
    await click('Edit');
    const fill = (value: string) => {
      const input = element.querySelector<HTMLTextAreaElement>('textarea[formControlName="nativeToolModels"]')!;
      input.value = value;
      input.dispatchEvent(new Event('input'));
      fixture.detectChanges();
    };
    expect(element.querySelector<HTMLTextAreaElement>('textarea')!.value).toBe('old-model');
    fill(' model-a\n\nmodel-a\nmodel-b ');
    await click('Save');
    expect(store.settings()!.ai.providers[0].nativeToolModels).toEqual(['model-a', 'model-b']);
    await click('Edit');
    fill('x'.repeat(257));
    await click('Save');
    expect(api.updateSettings).toHaveBeenCalledTimes(1);
    fill('');
    await click('Save');
    expect(store.settings()!.ai.providers[0].nativeToolModels).toEqual([]);
    await click('Edit');
    fill('model-c');
    const backend = element.querySelector<HTMLSelectElement>('select[formControlName="backend"]')!;
    backend.value = 'ollama';
    backend.dispatchEvent(new Event('change'));
    fixture.detectChanges();
    expect(element.querySelector('textarea[formControlName="nativeToolModels"]')).toBeNull();
    await click('Save');
    expect(store.settings()!.ai.providers[0].nativeToolModels).toEqual([]);
  });

  it('keeps keys out of settings and retries a failed key save without duplicating the provider', async () => {
    const api = {
      listAiModels: vi.fn(async () => []),
      getModelRoutes: vi.fn(async () => []),
      updateSettings: vi.fn(async (settings) => settings),
      setProviderApiKey: vi.fn().mockRejectedValueOnce(new Error('Vault locked')).mockResolvedValue(undefined),
      hasProviderApiKey: vi.fn(async () => true),
      deleteProviderApiKey: vi.fn(async () => undefined),
    };
    TestBed.configureTestingModule({ providers: [{ provide: JarvisApi, useValue: api }] });
    TestBed.inject(SettingsStore).settings.set(structuredClone(SETTINGS));
    const fixture = TestBed.createComponent(Models);
    fixture.detectChanges();
    await fixture.whenStable();
    const element = fixture.nativeElement as HTMLElement;
    const click = async (label: string) => {
      const button = [...element.querySelectorAll('button')].find((b) => b.textContent?.trim() === label);
      expect(button, label).toBeDefined();
      button!.click();
      fixture.detectChanges();
      await fixture.whenStable();
      await vi.waitFor(() => {
        fixture.detectChanges();
        expect(element.querySelector('fieldset')?.disabled ?? false).toBe(false);
      });
    };
    await click('+ Add');
    const fill = (name: string, value: string) => {
      const input = element.querySelector<HTMLInputElement>(`input[formControlName="${name}"]`)!;
      input.value = value;
      input.dispatchEvent(new Event('input'));
    };
    fill('displayName', 'My cloud');
    fill('baseUrl', 'https://example.com/v1');
    fill('apiKey', 'test-secret');
    await click('Save');
    expect(element.textContent).toContain('Vault locked');
    await click('Save');
    const configured = TestBed.inject(SettingsStore).settings()!.ai.providers;
    expect(configured.filter((p) => p.id === 'my-cloud')).toHaveLength(1);
    expect(JSON.stringify(api.updateSettings.mock.calls)).not.toContain('test-secret');
    expect(api.setProviderApiKey).toHaveBeenLastCalledWith('my-cloud', 'test-secret');
    expect(element.querySelector('input[type="password"]')).toBeNull();
    const row = [...element.querySelectorAll('.provider-list li')].find((li) => li.textContent?.includes('My cloud'))!;
    const edit = [...row.querySelectorAll('button')].find((b) => b.textContent?.trim() === 'Edit')!;
    edit.click();
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(element.querySelector<HTMLInputElement>('input[type="password"]')!.value).toBe('');
    await click('Delete saved API key');
    expect(api.deleteProviderApiKey).toHaveBeenCalledWith('my-cloud');
    await click('Save');
    expect(api.setProviderApiKey).toHaveBeenCalledTimes(2);
  });
});

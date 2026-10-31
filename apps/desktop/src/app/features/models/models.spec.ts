import type { ModelRouting, ProviderModels, ProviderTestResult, Settings } from '@jarvis/shared-types';
import { TestBed } from '@angular/core/testing';
import { JarvisApi } from '../../core/ipc/jarvis-api';
import { SettingsStore } from '../../core/state/settings.store';
import { MODEL_TEST_TIMEOUT_MS, ModelsStore, optionKey } from '../../core/state/models.store';
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

describe('Model routing selections', () => {
  function deferred<T>() {
    let resolve!: (value: T) => void;
    const promise = new Promise<T>((done) => {
      resolve = done;
    });
    return { promise, resolve };
  }

  function provider(vision = false): ProviderModels {
    return {
      provider: { id: 'local', displayName: 'Local', kind: 'localServer', locality: 'onDevice' },
      models: [
        {
          id: 'coder',
          displayName: 'Coder',
          providerId: 'local',
          contextWindow: 4096,
          capabilities: { chat: true, streaming: true, embeddings: false, toolCalling: false, vision },
        },
      ],
      error: null,
    };
  }

  async function render(catalog: ProviderModels[] = [], explicitGeneral = true) {
    const api = {
      listAiModels: vi.fn(async (): Promise<ProviderModels[]> => catalog),
      getModelRoutes: vi.fn(async () => []),
      updateSettings: vi.fn(async (settings: Settings) => settings),
    };
    TestBed.configureTestingModule({ providers: [{ provide: JarvisApi, useValue: api }] });
    const settings = TestBed.inject(SettingsStore);
    const initial: Settings = structuredClone(SETTINGS);
    initial.ai.routing.general = explicitGeneral ? { providerId: 'local', model: 'coder' } : null;
    settings.settings.set(initial);
    const fixture = TestBed.createComponent(Models);
    const models = TestBed.inject(ModelsStore);
    await models.refresh();
    fixture.detectChanges();
    const element = fixture.nativeElement as HTMLElement;
    const selection = (category: string) => element.querySelector<HTMLSelectElement>(`select[aria-label="${category} model"]`)!;
    const save = () => [...element.querySelectorAll('button')].find((button) => button.textContent?.trim() === 'Save routing');
    return { api, fixture, element, models, settings, selection, save };
  }

  afterEach(() => TestBed.resetTestingModule());

  it('keeps newer route edits while the submitted snapshot saves and blocks overlapping mutations', async () => {
    const available = provider();
    available.models.push({ ...available.models[0], id: 'other', displayName: 'Other' });
    const { api, fixture, element, settings, selection, save } = await render([available], false);
    const pending = deferred<Settings>();
    api.updateSettings.mockReturnValueOnce(pending.promise);
    const general = selection('General');
    general.value = optionKey('local', 'coder');
    general.dispatchEvent(new Event('change'));
    fixture.detectChanges();
    const submit = save()!;
    submit.click();
    fixture.detectChanges();
    expect(submit.disabled).toBe(true);
    expect(submit.textContent).toContain('Saving routing');
    const discard = [...element.querySelectorAll<HTMLButtonElement>('.routing button')].find(
      (button) => button.textContent?.trim() === 'Discard'
    )!;
    expect(discard.disabled).toBe(true);
    expect(general.disabled).toBe(false);

    general.value = optionKey('local', 'other');
    general.dispatchEvent(new Event('change'));
    // Dispatch directly as well as checking disabled state: handlers must guard
    // pending operations even if a second event is already queued.
    submit.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    discard.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    fixture.detectChanges();
    expect(api.updateSettings).toHaveBeenCalledOnce();
    expect(general.value).toBe(optionKey('local', 'other'));
    const submitted = api.updateSettings.mock.calls[0][0];
    expect(submitted.ai.routing.general).toEqual({ providerId: 'local', model: 'coder' });
    pending.resolve(submitted);
    await vi.waitFor(() => {
      fixture.detectChanges();
      expect(save()?.disabled).toBe(false);
    });
    expect(settings.settings()!.ai.routing.general).toEqual({ providerId: 'local', model: 'coder' });
    expect(general.value).toBe(optionKey('local', 'other'));
    save()!.click();
    await vi.waitFor(() => {
      fixture.detectChanges();
      expect(api.updateSettings).toHaveBeenCalledTimes(2);
      expect(save()).toBeUndefined();
    });
    expect(api.updateSettings.mock.calls[1][0].ai.routing.general).toEqual({ providerId: 'local', model: 'other' });
  });

  it('retains a refused route draft and permits an explicit retry', async () => {
    const { api, fixture, element, selection, save } = await render([provider()], false);
    api.updateSettings.mockRejectedValueOnce(new Error('Settings are not writable'));
    const general = selection('General');
    general.value = optionKey('local', 'coder');
    general.dispatchEvent(new Event('change'));
    fixture.detectChanges();
    save()!.click();
    await vi.waitFor(() => {
      fixture.detectChanges();
      expect(element.textContent).toContain('Settings are not writable');
      expect(save()?.disabled).toBe(false);
    });
    expect(general.value).toBe(optionKey('local', 'coder'));
    expect(api.updateSettings).toHaveBeenCalledOnce();
    save()!.click();
    await vi.waitFor(() => {
      fixture.detectChanges();
      expect(api.updateSettings).toHaveBeenCalledTimes(2);
      expect(save()).toBeUndefined();
    });
    expect(api.updateSettings.mock.calls[1][0].ai.routing.general).toEqual({ providerId: 'local', model: 'coder' });
  });

  it.each(['restore', 'automatic'] as const)('preserves a missing route until %s recovery is explicit', async (recovery) => {
    const { api, fixture, models, settings, selection, save } = await render();
    const general = selection('General');
    const key = optionKey('local', 'coder');
    expect(general.value).toBe(key);
    expect(general.selectedOptions[0]?.disabled).toBe(true);
    expect(general.selectedOptions[0]?.textContent).toContain('Selected model unavailable');
    expect(general.closest('li')?.querySelector('[role="status"]')?.textContent).toContain(
      'Choose another model or select Automatic'
    );
    general.dispatchEvent(new Event('change'));
    fixture.detectChanges();
    expect(save()).toBeUndefined();
    expect(settings.settings()!.ai.routing.general).toEqual({ providerId: 'local', model: 'coder' });

    if (recovery === 'restore') {
      const available = provider();
      api.listAiModels.mockResolvedValue([available]);
      models.providers.set([available]);
      fixture.detectChanges();
      expect(general.value).toBe(key);
      expect(general.selectedOptions[0]?.disabled).toBe(false);
      expect(general.selectedOptions[0]?.textContent).toContain('Coder');
      expect(general.closest('li')?.querySelector('[role="status"]')).toBeNull();
      const coding = selection('Coding');
      coding.value = key;
      coding.dispatchEvent(new Event('change'));
    } else {
      general.value = '';
      general.dispatchEvent(new Event('change'));
    }
    fixture.detectChanges();
    expect(save()).toBeDefined();
    save()!.click();
    await vi.waitFor(() => expect(api.updateSettings).toHaveBeenCalledOnce());
    const saved = api.updateSettings.mock.calls[0][0];
    expect(saved.ai.routing.general).toEqual(recovery === 'restore' ? { providerId: 'local', model: 'coder' } : null);
    if (recovery === 'restore') expect(saved.ai.routing.coding).toEqual({ providerId: 'local', model: 'coder' });
  });

  it.each(['General', 'Vision'] as const)('ignores a stale %s choice after its eligible model disappears', async (category) => {
    const { fixture, models, settings, selection, save } = await render([provider(true)], false);
    const select = selection(category);
    const key = optionKey('local', 'coder');
    expect([...select.options].some((option) => option.value === key)).toBe(true);
    // A catalog update can arrive before Angular removes the old DOM option.
    models.providers.set(category === 'General' ? [] : [provider(false)]);
    select.value = key;
    select.dispatchEvent(new Event('change'));
    fixture.detectChanges();
    expect(save()).toBeUndefined();
    expect(settings.settings()!.ai.routing[category === 'General' ? 'general' : 'vision']).toBeNull();
  });
});

describe('Provider connection tests', () => {
  const connected: ProviderTestResult = { reachable: true, locality: 'onDevice', latencyMs: 17, models: [], error: null };

  function deferred<T>() {
    let resolve!: (value: T) => void;
    let reject!: (error: unknown) => void;
    const promise = new Promise<T>((yes, no) => {
      resolve = yes;
      reject = no;
    });
    return { promise, resolve, reject };
  }

  async function render() {
    const api = {
      listAiModels: vi.fn(async () => []),
      getModelRoutes: vi.fn(async () => []),
      hasProviderApiKey: vi.fn(async () => false),
      testAiProvider: vi.fn(async (): Promise<ProviderTestResult> => ({ ...connected, models: [] })),
    };
    TestBed.configureTestingModule({ providers: [{ provide: JarvisApi, useValue: api }] });
    TestBed.inject(SettingsStore).settings.set(structuredClone(SETTINGS));
    const fixture = TestBed.createComponent(Models);
    fixture.detectChanges();
    await fixture.whenStable();
    const element = fixture.nativeElement as HTMLElement;
    const button = (label: string) => {
      const match = [...element.querySelectorAll('button')].find((item) => item.textContent?.trim() === label);
      expect(match, label).toBeDefined();
      return match!;
    };
    const click = (label: string) => {
      button(label).click();
      fixture.detectChanges();
    };
    const fill = (name: string, value: string) => {
      const input = element.querySelector<HTMLInputElement>(`input[formControlName="${name}"]`)!;
      input.value = value;
      input.dispatchEvent(new Event('input'));
      fixture.detectChanges();
    };
    click('+ Add');
    return { api, fixture, element, button, click, fill };
  }

  afterEach(() => TestBed.resetTestingModule());

  it.each(['', '   ', 'x'.repeat(65)])('explains an invalid provider name before contacting the server (%j)', async (name) => {
    const { api, element, click, fill } = await render();
    fill('displayName', name);
    click('Test connection');
    expect(element.querySelector('.field-error')?.textContent).toContain('Enter a name with 1–64 characters');
    expect(api.testAiProvider).not.toHaveBeenCalled();
  });

  it.each([' ', 'я'.repeat(4097), 'test-key\u0007'])('explains an invalid key without displaying it (%#)', async (key) => {
    const { api, element, click, fill } = await render();
    fill('displayName', 'Local');
    fill('apiKey', key);
    click('Test connection');
    expect(element.querySelector('.field-error')?.textContent).toContain('non-blank key of at most 8192 bytes');
    if (key.trim()) expect(element.textContent).not.toContain(key);
    expect(api.testAiProvider).not.toHaveBeenCalled();
  });

  it.each(['displayName', 'baseUrl'] as const)(
    'accepts a 64-character Unicode name and clears success after %s edits',
    async (name) => {
      const { api, fixture, element, click, fill } = await render();
      fill('displayName', ` ${'😀'.repeat(64)} `);
      click('Test connection');
      await vi.waitFor(() => {
        fixture.detectChanges();
        expect(element.querySelector('.test-result')?.textContent).toContain('Connected in 17 ms');
        expect(element.querySelector('fieldset')?.disabled).toBe(false);
      });
      expect(api.testAiProvider).toHaveBeenCalledWith(expect.objectContaining({ displayName: '😀'.repeat(64) }), undefined);
      fill(name, name === 'displayName' ? 'Renamed' : 'http://127.0.0.1:8080');
      expect(element.querySelector('.test-result')).toBeNull();
    }
  );

  it('ignores an old saved-key lookup after reopening the same provider', async () => {
    const { api, fixture, element, click } = await render();
    const oldKey = deferred<boolean>();
    api.hasProviderApiKey.mockReturnValueOnce(oldKey.promise);
    click('Cancel');
    click('Edit');
    click('Cancel');
    click('Edit');
    await fixture.whenStable();
    oldKey.resolve(true);
    await oldKey.promise;
    await fixture.whenStable();
    fixture.detectChanges();
    expect(api.hasProviderApiKey).toHaveBeenCalledTimes(2);
    expect(element.textContent).not.toContain('Delete saved API key');
  });

  it.each(['success', 'failure'] as const)('cancels waiting and ignores old %s while a new test is pending', async (outcome) => {
    const { api, fixture, element, button, click, fill } = await render();
    const first = deferred<Awaited<ReturnType<typeof api.testAiProvider>>>();
    const second = deferred<Awaited<ReturnType<typeof api.testAiProvider>>>();
    api.testAiProvider.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    fill('displayName', 'First');
    fill('apiKey', 'temporary-test-key');
    click('Test connection');
    expect(element.querySelector<HTMLFieldSetElement>('fieldset')?.disabled).toBe(true);
    expect(button('Cancel').disabled).toBe(false);
    click('Cancel');
    expect(element.querySelector('.provider-form')).toBeNull();

    click('+ Add');
    expect(element.querySelector<HTMLInputElement>('input[formControlName="apiKey"]')?.value).toBe('');
    fill('displayName', 'Second');
    click('Test connection');
    expect(api.testAiProvider).toHaveBeenCalledTimes(2);
    if (outcome === 'success') first.resolve({ ...connected, models: [], latencyMs: 999 });
    else first.reject(new Error('Old connection error'));
    await first.promise.catch(() => undefined);
    await fixture.whenStable();
    fixture.detectChanges();
    expect(element.querySelector('.test-result')).toBeNull();
    expect(element.querySelector<HTMLFieldSetElement>('fieldset')?.disabled).toBe(true);
    expect(button('Testing…').disabled).toBe(true);
    second.resolve({ ...connected, models: [], latencyMs: 23 });
    await vi.waitFor(() => {
      fixture.detectChanges();
      expect(element.querySelector('.test-result')?.textContent).toContain('Connected in 23 ms');
      expect(element.querySelector('fieldset')?.disabled).toBe(false);
    });
  });

  it('shows a timed-out connection test and makes the form usable again', async () => {
    const { api, fixture, element, button, click, fill } = await render();
    vi.useFakeTimers();
    try {
      api.testAiProvider.mockReturnValueOnce(new Promise(() => undefined));
      fill('displayName', 'Local');
      click('Test connection');
      await vi.advanceTimersByTimeAsync(MODEL_TEST_TIMEOUT_MS);
      fixture.detectChanges();
      expect(element.querySelector('.test-result.bad')?.textContent).toContain('did not respond within 30 seconds');
      expect(element.querySelector('fieldset')?.disabled).toBe(false);
      expect(button('Test connection').disabled).toBe(false);
    } finally {
      TestBed.resetTestingModule();
      vi.useRealTimers();
    }
  });
});

describe('Provider credentials form', () => {
  it('serializes the provider and key save across catalog refresh and snapshots the submitted key', async () => {
    let finishCatalog!: (providers: ProviderModels[]) => void;
    const pendingCatalog = new Promise<ProviderModels[]>((resolve) => {
      finishCatalog = resolve;
    });
    const api = {
      listAiModels: vi.fn(async (): Promise<ProviderModels[]> => []),
      getModelRoutes: vi.fn(async () => []),
      updateSettings: vi.fn(async (settings: Settings) => settings),
      setProviderApiKey: vi.fn(async () => undefined),
      hasProviderApiKey: vi.fn(async () => false),
    };
    TestBed.configureTestingModule({ providers: [{ provide: JarvisApi, useValue: api }] });
    const settings = TestBed.inject(SettingsStore);
    settings.settings.set(structuredClone(SETTINGS));
    const fixture = TestBed.createComponent(Models);
    await TestBed.inject(ModelsStore).refresh();
    fixture.detectChanges();
    const element = fixture.nativeElement as HTMLElement;
    const button = (label: string, root: ParentNode = element) =>
      [...root.querySelectorAll<HTMLButtonElement>('button')].find((item) => item.textContent?.trim() === label)!;
    button('Remove').click();
    fixture.detectChanges();
    button('+ Add').click();
    fixture.detectChanges();
    const fill = (name: string, value: string) => {
      const input = element.querySelector<HTMLInputElement>(`input[formControlName="${name}"]`)!;
      input.value = value;
      input.dispatchEvent(new Event('input'));
      fixture.detectChanges();
    };
    fill('displayName', 'New server');
    fill('apiKey', 'submitted-test-key');
    api.listAiModels.mockReturnValueOnce(pendingCatalog);
    button('Save').click();
    await vi.waitFor(() => {
      fixture.detectChanges();
      expect(api.listAiModels).toHaveBeenCalledTimes(2);
    });
    // The settings write is finished, but its full provider/key pipeline is not.
    expect(settings.saving()).toBe(false);
    const newRow = [...element.querySelectorAll<HTMLLIElement>('.provider-list li')].find((row) =>
      row.textContent?.includes('New server')
    )!;
    for (const control of [button('Yes'), button('Disable', newRow), button('Remove', newRow), button('Cancel')]) {
      expect(control.disabled).toBe(true);
      control.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    }
    fill('apiKey', 'later-test-key');
    expect(api.updateSettings).toHaveBeenCalledOnce();
    expect(api.setProviderApiKey).not.toHaveBeenCalled();
    finishCatalog([]);
    await vi.waitFor(() => {
      fixture.detectChanges();
      expect(element.querySelector('.provider-form')).toBeNull();
    });
    expect(api.setProviderApiKey).toHaveBeenCalledOnce();
    expect(api.setProviderApiKey).toHaveBeenCalledWith('new-server', 'submitted-test-key');
    expect(settings.settings()!.ai.providers.some((config) => config.id === 'new-server')).toBe(true);
    expect(api.updateSettings).toHaveBeenCalledOnce();
    await vi.waitFor(() => {
      fixture.detectChanges();
      expect(button('Edit', newRow).disabled).toBe(false);
    });
    button('Edit', newRow).click();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(element.querySelector('.provider-form')).not.toBeNull();
    button('Remove', newRow).click();
    fixture.detectChanges();
    button('Yes', newRow).click();
    await vi.waitFor(() => {
      fixture.detectChanges();
      expect(api.updateSettings).toHaveBeenCalledTimes(2);
      expect(element.querySelector('.provider-form')).toBeNull();
    });
    expect(settings.settings()!.ai.providers.some((config) => config.id === 'new-server')).toBe(false);
    expect(api.setProviderApiKey).toHaveBeenCalledOnce();
  });

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

  it('connects a subscription CLI with its fixed address and no API key', async () => {
    const api = {
      listAiModels: vi.fn(async () => []),
      getModelRoutes: vi.fn(async () => []),
      updateSettings: vi.fn(async (settings) => settings),
      setProviderApiKey: vi.fn(async () => undefined),
      hasProviderApiKey: vi.fn(async () => false),
      testAiProvider: vi.fn(async (): Promise<ProviderTestResult> => ({
        reachable: true,
        locality: 'thirdPartyCloud',
        latencyMs: 5,
        models: [],
        error: null,
      })),
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
    const name = element.querySelector<HTMLInputElement>('input[formControlName="displayName"]')!;
    name.value = 'Claude';
    name.dispatchEvent(new Event('input'));
    const key = element.querySelector<HTMLInputElement>('input[formControlName="apiKey"]')!;
    key.value = 'typed-before-switching';
    key.dispatchEvent(new Event('input'));
    const backend = element.querySelector<HTMLSelectElement>('select[formControlName="backend"]')!;
    backend.value = 'claudeCode';
    backend.dispatchEvent(new Event('change'));
    fixture.detectChanges();
    expect(element.querySelector('input[formControlName="baseUrl"]')).toBeNull();
    expect(element.querySelector('input[formControlName="apiKey"]')).toBeNull();
    expect(element.textContent).toContain('claude auth login');
    await click('Test connection');
    expect(api.testAiProvider).toHaveBeenCalledWith(
      expect.objectContaining({ backend: 'claudeCode', baseUrl: 'https://claude.ai' }),
      undefined
    );
    await click('Save');
    const saved = TestBed.inject(SettingsStore)
      .settings()!
      .ai.providers.find((p) => p.id === 'claude')!;
    expect(saved).toMatchObject({ backend: 'claudeCode', baseUrl: 'https://claude.ai' });
    expect(api.setProviderApiKey).not.toHaveBeenCalled();
  });
});

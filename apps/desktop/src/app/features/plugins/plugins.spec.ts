import { TestBed } from '@angular/core/testing';

import { JarvisApi } from '../../core/ipc/jarvis-api';
import { pluginInfo, pluginReview, pluginTool } from '../../testing/plugins.fixture';
import { PluginsPage, serverRequest, statusLabel } from './plugins';

describe('PluginsPage', () => {
  const api = {
    onPlugins: vi.fn(async () => () => undefined),
    listPlugins: vi.fn(async () => [
      pluginInfo({
        secrets: [{ name: 'API_KEY', description: 'Key from example.com', set: false }],
        tools: [
          pluginTool('search', { hints: { readOnly: true, destructive: null, idempotent: null, openWorld: null } }),
          pluginTool('delete', { risk: 'high', riskReset: true }),
        ],
      }),
    ]),
    reviewMcpServer: vi.fn(async () => pluginReview()),
    installPlugin: vi.fn(async () => pluginInfo()),
    setPluginTool: vi.fn(async () => pluginInfo()),
    setPluginSecret: vi.fn(async () =>
      pluginInfo({ secrets: [{ name: 'API_KEY', description: 'Key from example.com', set: true }] })
    ),
    removePlugin: vi.fn(async () => undefined),
  };

  async function render() {
    TestBed.configureTestingModule({
      imports: [PluginsPage],
      providers: [{ provide: JarvisApi, useValue: api }],
    });
    const fixture = TestBed.createComponent(PluginsPage);
    await fixture.whenStable();
    fixture.detectChanges();
    return { fixture, element: fixture.nativeElement as HTMLElement };
  }

  function button(element: HTMLElement, text: string): HTMLButtonElement {
    const found = [...element.querySelectorAll('button')].find((b) => b.textContent?.trim().startsWith(text));
    if (!found) {
      throw new Error(`no button "${text}"`);
    }
    return found;
  }

  beforeEach(() => vi.clearAllMocks());

  it('shows plugins with tools, hints and the risk that applies', async () => {
    const { element } = await render();
    expect(element.textContent).toContain('Notes');
    expect(element.textContent).toContain('Running');
    expect(element.textContent).toContain('read-only');
    expect(element.textContent).toContain('changed since you lowered its risk');
    expect(element.textContent).toContain('does not sandbox it');
  });

  it('reviews an MCP server before adding it', async () => {
    const { fixture, element } = await render();
    button(element, 'Add an MCP server').click();
    fixture.detectChanges();
    const inputs = element.querySelectorAll<HTMLInputElement>('form input');
    inputs[0].value = 'My server';
    inputs[0].dispatchEvent(new Event('input'));
    inputs[1].value = 'npx';
    inputs[1].dispatchEvent(new Event('input'));
    button(element, 'Review').click();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(api.reviewMcpServer).toHaveBeenCalled();
    expect(api.installPlugin).not.toHaveBeenCalled();
    expect(element.textContent).toContain('cannot check what it runs');
    expect(element.textContent).toContain('API_KEY');

    button(element, 'Add and start').click();
    await fixture.whenStable();
    expect(api.installPlugin).toHaveBeenCalledWith('review-1');
  });

  it('asks before removing a plugin', async () => {
    const { fixture, element } = await render();
    const panel = element.querySelector('.plugin') as HTMLElement;
    button(panel, 'Remove').click();
    fixture.detectChanges();
    expect(api.removePlugin).not.toHaveBeenCalled();
    expect(panel.textContent).toContain('Its folder stays');
    const confirm = [...panel.querySelectorAll('button.danger')][0] as HTMLButtonElement;
    confirm.click();
    await fixture.whenStable();
    expect(api.removePlugin).toHaveBeenCalledWith('notes');
  });

  it('stores secrets without showing them again', async () => {
    const { fixture, element } = await render();
    const input = element.querySelector<HTMLInputElement>('.secret input')!;
    expect(input.type).toBe('password');
    input.value = 's3cret';
    input.dispatchEvent(new Event('input'));
    fixture.detectChanges();
    button(element.querySelector('.secret') as HTMLElement, 'Save').click();
    await new Promise((resolve) => setTimeout(resolve));
    fixture.detectChanges();
    expect(api.setPluginSecret).toHaveBeenCalledWith('notes', 'API_KEY', 's3cret');
    expect(input.value).toBe('');
    expect(element.querySelector('.secret')?.textContent).toContain('stored in the keychain');
  });
});

describe('plugin helpers', () => {
  it('parses the server form', () => {
    expect(
      serverRequest({
        name: ' Files ',
        program: 'npx',
        args: '-y\n\n pkg ',
        cwd: ' ',
        env: 'MODE=fast\nURL=a=b',
        secrets: 'KEY, TOKEN',
      })
    ).toEqual({
      name: 'Files',
      program: 'npx',
      args: ['-y', 'pkg'],
      cwd: null,
      env: [
        { name: 'MODE', value: 'fast' },
        { name: 'URL', value: 'a=b' },
      ],
      secretNames: ['KEY', 'TOKEN'],
    });
    expect(serverRequest({ name: 'x', program: 'x', args: '', cwd: '', env: 'BROKEN', secrets: '' })).toBe(
      '"BROKEN" is not NAME=value'
    );
  });

  it('labels statuses', () => {
    expect(statusLabel({ state: 'failed', error: 'x', retryInSecs: 10 })).toBe('Restarting in 10 s');
    expect(statusLabel({ state: 'failed', error: 'x', retryInSecs: null })).toBe('Stopped');
    expect(statusLabel({ state: 'needsSecrets', names: ['A'] })).toBe('Needs secrets');
  });
});

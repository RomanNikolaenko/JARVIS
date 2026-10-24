import { BehaviorSubject } from 'rxjs';
import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import type { Settings } from '@jarvis/shared-types';

import { JarvisApi } from '../../core/ipc/jarvis-api';
import { AgentsStore, summarize } from '../../core/state/agents.store';
import { SettingsStore } from '../../core/state/settings.store';
import { AGENT_STATUS, agentTask, auditEntry, tool } from '../../testing/agents.fixture';
import { SETTINGS } from '../../testing/settings.fixture';
import { AgentsPage } from './agents';

describe('AgentsPage', () => {
  let saved: Settings[];
  const api = {
    connected: true,
    onAgentTask: vi.fn(async () => () => undefined),
    onApprovals: vi.fn(async () => () => undefined),
    listPendingApprovals: vi.fn(async () => []),
    getAgentStatus: vi.fn(async () => AGENT_STATUS),
    listTools: vi.fn(async () => [
      tool('read_file', 'low', true),
      tool('write_file', 'medium', false),
      tool('delete_path', 'high', false),
    ]),
    listAgentTasks: vi.fn(async () => [summarize(agentTask('t1', 'failed'))]),
    getAgentTask: vi.fn(async (id: string) => agentTask(id, 'failed')),
    startAgentTask: vi.fn(async () => agentTask('t2', 'pending', 9)),
    listAiModels: vi.fn(async () => []),
    getModelRoutes: vi.fn(async () => []),
    getAuditLog: vi.fn(async () => [auditEntry('e1', 5)]),
    verifyAuditLog: vi.fn(async () => ({ entries: 1, intact: false, brokenAt: 'e1' })),
    updateSettings: vi.fn(async (settings: Settings) => {
      saved.push(settings);
      return settings;
    }),
    addWorkspaceFolder: vi.fn(async (): Promise<Settings> => ({
      ...SETTINGS,
      tools: { ...SETTINGS.tools, workspaceFolders: ['/work', '/new'] },
    })),
  };

  async function render() {
    saved = [];
    TestBed.configureTestingModule({
      imports: [AgentsPage],
      providers: [provideRouter([]), { provide: JarvisApi, useValue: api }],
    });
    TestBed.inject(SettingsStore).settings.set({
      ...SETTINGS,
      tools: { ...SETTINGS.tools, workspaceFolders: ['/work'] },
    });
    const fixture = TestBed.createComponent(AgentsPage);
    await fixture.whenStable();
    fixture.detectChanges();
    return { fixture, element: fixture.nativeElement as HTMLElement };
  }

  async function settle(fixture: { whenStable(): Promise<unknown>; detectChanges(): void }) {
    await fixture.whenStable();
    fixture.detectChanges();
  }

  function button(element: HTMLElement, text: string): HTMLButtonElement {
    const found = [...element.querySelectorAll('button')].find((b) => b.textContent?.includes(text));
    if (!found) {
      throw new Error(`no button "${text}"`);
    }
    return found;
  }

  it('shows the selected task with its journal and error', async () => {
    const { element } = await render();
    expect(element.querySelector('.task-goal')?.textContent).toContain('goal t1');
    expect(element.querySelector('.detail-header h2')?.textContent).toContain('goal t1');
    expect(element.querySelector('.step-title')?.textContent).toContain('list_directory');
    expect(element.textContent).toContain('allowed (read-only)');
    expect(element.querySelector('.notice.danger')?.textContent).toContain('step limit');
  });

  it('keeps a connection error visible and retries from the page', async () => {
    api.onApprovals.mockRejectedValueOnce(new Error('Subscription unavailable'));
    const { fixture, element } = await render();
    await vi.waitFor(() => {
      fixture.detectChanges();
      expect(element.textContent).toContain('Subscription unavailable');
    });
    const store = TestBed.inject(AgentsStore);
    await store.loadAll();
    fixture.detectChanges();
    expect(element.textContent).toContain('Subscription unavailable');
    button(element, 'Retry connection').click();
    await vi.waitFor(() => {
      fixture.detectChanges();
      expect(element.textContent).not.toContain('Subscription unavailable');
    });
    expect(store.subscriptionError()).toBeNull();
  });

  it('starts a task with the goal and limits', async () => {
    const { fixture, element } = await render();
    const goal = element.querySelector('textarea') as HTMLTextAreaElement;
    goal.value = 'Count the files';
    goal.dispatchEvent(new Event('input'));
    await settle(fixture);
    button(element, 'Start task').click();
    await settle(fixture);
    expect(api.startAgentTask).toHaveBeenCalledWith({
      goal: 'Count the files',
      providerId: null,
      model: null,
      maxSteps: null,
      maxDurationSecs: null,
    });
    expect(element.querySelector('.detail-header h2')?.textContent).toContain('goal t2');
  });

  it('saves tool permissions and allowed programs', async () => {
    const { fixture, element } = await render();
    button(element, 'Tools & permissions').click();
    await settle(fixture);
    expect(element.textContent).toContain('/work');

    const writeFile = [...element.querySelectorAll('label.tool')].find((l) => l.textContent?.includes('write_file'));
    const checkbox = writeFile?.querySelector('input') as HTMLInputElement;
    checkbox.checked = true;
    checkbox.dispatchEvent(new Event('change'));
    const programs = element.querySelector('input[placeholder="git, cargo, npm"]') as HTMLInputElement;
    programs.value = 'git, cargo,git';
    programs.dispatchEvent(new Event('input'));
    await settle(fixture);

    button(element, 'Save').click();
    await settle(fixture);
    expect(saved).toHaveLength(1);
    expect(saved[0].tools.enabled).toContain('write_file');
    expect(saved[0].tools.allowedPrograms).toEqual(['git', 'cargo']);
    expect(saved[0].privacy.allowCloudAgents).toBe(false);
  });

  // Seen live: the added folder did not show up, and a later Save dropped it.
  it('shows an added folder at once and keeps it when saving', async () => {
    const { fixture, element } = await render();
    button(element, 'Tools & permissions').click();
    await settle(fixture);
    button(element, 'Add folder').click();
    await settle(fixture);
    expect(element.querySelector('.folders')?.textContent).toContain('/new');

    const writeFile = [...element.querySelectorAll('label.tool')].find((l) => l.textContent?.includes('write_file'));
    const checkbox = writeFile?.querySelector('input') as HTMLInputElement;
    checkbox.checked = true;
    checkbox.dispatchEvent(new Event('change'));
    await settle(fixture);
    button(element, 'Save').click();
    await settle(fixture);
    expect(saved.at(-1)?.tools.workspaceFolders).toEqual(['/work', '/new']);
    expect(saved.at(-1)?.tools.enabled).toContain('write_file');
  });

  it('lists audit entries and reports a broken chain', async () => {
    const { fixture, element } = await render();
    button(element, 'Audit log').click();
    await settle(fixture);
    expect(element.querySelector('tbody')?.textContent).toContain('Read a.txt');
    button(element, 'Check integrity').click();
    await settle(fixture);
    expect(element.querySelector('.verdict.bad')?.textContent).toContain('e1');
  });

  it('shows audit loading and failure states and retries without claiming the log is empty', async () => {
    let reject!: (reason: unknown) => void;
    api.getAuditLog.mockReturnValueOnce(
      new Promise((_resolve, fail) => {
        reject = fail;
      })
    );
    const { fixture, element } = await render();
    button(element, 'Audit log').click();
    await settle(fixture);
    expect(element.textContent).toContain('Loading audit entries');
    expect(element.textContent).not.toContain('No tool calls yet');
    reject(new Error('audit unavailable'));
    await TestBed.inject(AgentsStore).loadAudit();
    await settle(fixture);
    expect(element.querySelector('jv-audit-panel [role="alert"]')?.textContent).toContain('audit unavailable');
    expect(element.textContent).not.toContain('No tool calls yet');
    button(element, 'Refresh').click();
    await TestBed.inject(AgentsStore).loadAudit();
    await settle(fixture);
    expect(element.querySelector('tbody')?.textContent).toContain('Read a.txt');
    expect(element.querySelector('jv-audit-panel [role="alert"]')).toBeNull();
  });

  it('shows integrity progress, removes the previous verdict and retries a failed check', async () => {
    const { fixture, element } = await render();
    button(element, 'Audit log').click();
    await settle(fixture);
    const store = TestBed.inject(AgentsStore);
    await store.verifyAudit();
    await settle(fixture);
    expect(element.textContent).toContain('Last check:');
    let reject!: (reason: unknown) => void;
    api.verifyAuditLog.mockReturnValueOnce(
      new Promise((_resolve, fail) => {
        reject = fail;
      })
    );
    button(element, 'Check integrity').click();
    const pending = store.verifyAudit();
    await settle(fixture);
    expect(button(element, 'Check integrity').disabled).toBe(true);
    expect(button(element, 'Refresh').disabled).toBe(false);
    expect(element.textContent).toContain('Checking audit integrity');
    expect(element.textContent).not.toContain('Last check:');
    reject(new Error('integrity check unavailable'));
    await pending;
    await settle(fixture);
    expect(element.querySelector('jv-audit-panel [role="alert"]')?.textContent).toContain('integrity check unavailable');
    expect(button(element, 'Check integrity').disabled).toBe(false);
    button(element, 'Check integrity').click();
    await store.verifyAudit();
    await settle(fixture);
    expect(element.textContent).not.toContain('integrity check unavailable');
    expect(element.textContent).toContain('Last check:');
  });

  it('keeps edits made while choosing a folder and saves them with the added folder', async () => {
    let finish!: (settings: Settings) => void;
    api.addWorkspaceFolder.mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      })
    );
    const { fixture, element } = await render();
    button(element, 'Tools & permissions').click();
    await settle(fixture);
    const before = api.addWorkspaceFolder.mock.calls.length;
    const saveButton = button(element, 'Save');
    button(element, 'Add folder').click();
    fixture.detectChanges();
    expect(button(element, 'Add folder').disabled).toBe(true);
    expect(saveButton.disabled).toBe(true);
    const writeFile = [...element.querySelectorAll('label.tool')].find((label) => label.textContent?.includes('write_file'));
    const checkbox = writeFile?.querySelector('input') as HTMLInputElement;
    checkbox.checked = true;
    checkbox.dispatchEvent(new Event('change'));
    fixture.detectChanges();
    expect(saveButton.disabled).toBe(true);
    finish({ ...SETTINGS, tools: { ...SETTINGS.tools, workspaceFolders: ['/work', '/new'] } });
    await settle(fixture);
    await vi.waitFor(() => {
      fixture.detectChanges();
      expect(element.querySelector('.folders')?.textContent).toContain('/new');
    });
    expect(api.addWorkspaceFolder.mock.calls.length - before).toBe(1);
    expect(element.querySelector('.folders')?.textContent).toContain('/new');
    expect(checkbox.checked).toBe(true);
    expect(button(element, 'Save').disabled).toBe(false);
    button(element, 'Save').click();
    await settle(fixture);
    expect(saved.at(-1)?.tools.workspaceFolders).toEqual(['/work', '/new']);
    expect(saved.at(-1)?.tools.enabled).toContain('write_file');
  });
});

describe('Agent task links', () => {
  it('opens the requested task after loading and when the URL changes', async () => {
    const params = new BehaviorSubject(convertToParamMap({ task: 'from-chat' }));
    let loaded!: () => void;
    const store = {
      start: vi.fn(),
      loadAll: vi.fn(
        () =>
          new Promise<void>((resolve) => {
            loaded = resolve;
          })
      ),
      select: vi.fn(),
    };
    TestBed.configureTestingModule({
      providers: [
        { provide: AgentsStore, useValue: store },
        { provide: ActivatedRoute, useValue: { queryParamMap: params } },
      ],
    });
    TestBed.overrideComponent(AgentsPage, { set: { template: '', imports: [] } });
    const fixture = TestBed.createComponent(AgentsPage);
    fixture.detectChanges();
    expect(store.select).not.toHaveBeenCalled();
    loaded();
    await fixture.whenStable();
    expect(store.select).toHaveBeenCalledWith('from-chat');
    params.next(convertToParamMap({ task: 'another-task' }));
    expect(store.select).toHaveBeenLastCalledWith('another-task');
    fixture.destroy();
    params.next(convertToParamMap({ task: 'after-destroy' }));
    expect(store.select).toHaveBeenCalledTimes(2);
  });
});

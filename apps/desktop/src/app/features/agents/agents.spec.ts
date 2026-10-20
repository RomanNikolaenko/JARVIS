import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import type { Settings } from '@jarvis/shared-types';

import { JarvisApi } from '../../core/ipc/jarvis-api';
import { summarize } from '../../core/state/agents.store';
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
    const found = [...element.querySelectorAll('button')].find((b) =>
      b.textContent?.includes(text),
    );
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

    const writeFile = [...element.querySelectorAll('label.tool')].find((l) =>
      l.textContent?.includes('write_file'),
    );
    const checkbox = writeFile?.querySelector('input') as HTMLInputElement;
    checkbox.checked = true;
    checkbox.dispatchEvent(new Event('change'));
    const programs = element.querySelector(
      'input[placeholder="git, cargo, npm"]',
    ) as HTMLInputElement;
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

  it('lists audit entries and reports a broken chain', async () => {
    const { fixture, element } = await render();
    button(element, 'Audit log').click();
    await settle(fixture);
    expect(element.querySelector('tbody')?.textContent).toContain('Read a.txt');
    button(element, 'Check integrity').click();
    await settle(fixture);
    expect(element.querySelector('.verdict.bad')?.textContent).toContain('e1');
  });
});

import { BehaviorSubject } from 'rxjs';
import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import type { ProviderModels, Settings } from '@jarvis/shared-types';

import { JarvisApi } from '../../core/ipc/jarvis-api';
import { AgentsStore, summarize } from '../../core/state/agents.store';
import { ModelsStore, optionKey } from '../../core/state/models.store';
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
    resumeAgentTask: vi.fn(async (_id: string) => undefined),
    deleteReviewedAgentTask: vi.fn(async () => 1),
    listAiModels: vi.fn(async (): Promise<ProviderModels[]> => []),
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

  function actionOutput(element: HTMLElement): HTMLDetailsElement | undefined {
    return [...element.querySelectorAll<HTMLDetailsElement>('.step details')].find(
      (details) => details.querySelector('summary')?.textContent?.trim() === 'Output'
    );
  }

  async function selectPausedTask(fixture: { detectChanges(): void }, task = agentTask('paused', 'paused')) {
    const models = TestBed.inject(ModelsStore);
    await models.refresh();
    const original: ProviderModels = {
      provider: { id: 'local-llama', displayName: 'Original server', kind: 'localServer', locality: 'onDevice' },
      models: [
        {
          id: 'qwen',
          displayName: 'Qwen',
          providerId: 'local-llama',
          contextWindow: 4096,
          capabilities: { chat: true, streaming: true, embeddings: false, toolCalling: false, vision: false },
        },
      ],
      error: null,
    };
    models.providers.set([original]);
    const store = TestBed.inject(AgentsStore);
    await vi.waitFor(() => expect(store.busy()).toBe(false));
    store.selectedId.set(task.id);
    store.selected.set(task);
    fixture.detectChanges();
    api.resumeAgentTask.mockClear();
    api.startAgentTask.mockClear();
    return { models, original, store, task };
  }

  it('keeps a paused journal and draft while its original model is missing, then resumes only that task after refresh', async () => {
    const { fixture, element } = await render();
    const { models, original, store, task } = await selectPausedTask(fixture);
    const journal = structuredClone(task);
    const goal = element.querySelector<HTMLTextAreaElement>('textarea[name="goal"]')!;
    goal.value = 'Keep this new task draft';
    goal.dispatchEvent(new Event('input'));
    fixture.detectChanges();
    expect(button(element, 'Resume').disabled).toBe(false);
    models.providers.set([]);
    fixture.detectChanges();
    expect(button(element, 'Resume').disabled).toBe(true);
    expect(element.querySelector('.resume-notice')?.textContent).toContain('qwen');
    expect(element.querySelector('.resume-notice')?.textContent).toContain('local-llama');
    button(element, 'Resume').dispatchEvent(new MouseEvent('click', { bubbles: true }));
    expect(api.resumeAgentTask).not.toHaveBeenCalled();
    expect(api.startAgentTask).not.toHaveBeenCalled();
    expect(store.selected()).toEqual(journal);
    expect(goal.value).toBe('Keep this new task draft');
    expect(button(element, 'Cancel').disabled).toBe(false);

    api.listAiModels.mockResolvedValueOnce([original]);
    button(element, 'Refresh models').click();
    await vi.waitFor(() => {
      fixture.detectChanges();
      expect(button(element, 'Resume').disabled).toBe(false);
    });
    expect(api.listAiModels).toHaveBeenLastCalledWith(true);
    button(element, 'Resume').click();
    await vi.waitFor(() => expect(api.resumeAgentTask).toHaveBeenCalledWith(task.id));
    expect(api.resumeAgentTask).toHaveBeenCalledOnce();
    expect(api.startAgentTask).not.toHaveBeenCalled();
    expect(store.selected()).toEqual(journal);
    expect(goal.value).toBe('Keep this new task draft');
  });

  it('uses the current agent permission rather than a stale enabled status when resuming', async () => {
    const { fixture, element } = await render();
    const { task, store } = await selectPausedTask(fixture);
    const settings = TestBed.inject(SettingsStore);
    settings.settings.update((current) => ({ ...current!, agents: { ...current!.agents, enabled: false } }));
    fixture.detectChanges();
    expect(store.status()?.enabled).toBe(true);
    expect(button(element, 'Resume').disabled).toBe(true);
    expect(element.querySelector('.resume-notice')?.textContent).toContain('Agents are turned off');
    expect(button(element, 'Cancel').disabled).toBe(false);
    button(element, 'Resume').dispatchEvent(new MouseEvent('click', { bubbles: true }));
    expect(api.resumeAgentTask).not.toHaveBeenCalled();

    settings.settings.update((current) => ({ ...current!, agents: { ...current!.agents, enabled: true } }));
    fixture.detectChanges();
    expect(button(element, 'Resume').disabled).toBe(false);
    button(element, 'Resume').click();
    await vi.waitFor(() => expect(api.resumeAgentTask).toHaveBeenCalledWith(task.id));
  });

  it.each(['provider', 'model', 'unknown', 'inProgress'] as const)(
    'refuses to resume a paused journal with %s missing or unresolved',
    async (issue) => {
      const { fixture, element } = await render();
      const task = agentTask('cannot-resume', 'paused');
      if (issue === 'provider') task.providerId = null;
      else if (issue === 'model') task.model = null;
      else task.steps[0].outcome = issue;
      const { store } = await selectPausedTask(fixture, task);
      expect(button(element, 'Resume').disabled).toBe(true);
      expect(element.querySelector('.resume-notice')?.textContent).toContain(
        issue === 'provider' || issue === 'model' ? 'no complete record of its original model' : 'unknown result'
      );
      button(element, 'Resume').dispatchEvent(new MouseEvent('click', { bubbles: true }));
      expect(api.resumeAgentTask).not.toHaveBeenCalled();
      expect(api.startAgentTask).not.toHaveBeenCalled();
      expect(store.selected()).toBe(task);
      expect(button(element, 'Cancel').disabled).toBe(false);
    }
  );

  it.each(['task', 'model'] as const)('ignores a stale Resume click after the selected %s changes', async (changed) => {
    const { fixture, element } = await render();
    const { task, store, models, original } = await selectPausedTask(fixture);
    const oldResume = button(element, 'Resume');
    const current = structuredClone(task);
    if (changed === 'task') current.id = 'other-task';
    else {
      current.model = 'new-model';
      models.providers.set([{ ...original, models: [{ ...original.models[0], id: 'new-model' }] }]);
    }
    store.selectedId.set(current.id);
    store.selected.set(current);
    // Keep the old rendered event binding to exercise the imperative guard.
    oldResume.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    expect(api.resumeAgentTask).not.toHaveBeenCalled();
    expect(store.selected()).toBe(current);
    expect(store.selectedId()).toBe(current.id);
  });

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

  it('shows an unverified answer as a warning without resume or cancel controls', async () => {
    const { fixture, element } = await render();
    const task = agentTask('unverified', 'unverified');
    task.result = 'Candidate answer';
    task.error = 'The answer could not be verified. Review it before relying on it.';
    task.steps[0].kind = 'verification';
    task.steps[0].outcome = 'unverified';
    TestBed.inject(AgentsStore).selected.set(task);
    fixture.detectChanges();
    expect(element.querySelector('.detail-meta')?.textContent).toContain('Answer unverified');
    expect(element.querySelector('.notice.warning')?.textContent).toContain('could not be verified');
    expect(element.querySelector('.result')?.textContent).toContain('Candidate answer');
    expect(element.querySelector('.chip.warning')?.textContent).toContain('unverified');
    expect(element.querySelector('.controls')?.textContent?.trim()).toBe('');
  });

  it('shows an unknown action result and requires manual review', async () => {
    const { fixture, element } = await render();
    const task = agentTask('recovered', 'failed');
    task.error = 'Operation recovered:0 has no durable result. Check its effects manually.';
    task.steps[0].outcome = 'unknown';
    task.steps[0].output = 'The write may have completed before JARVIS stopped.';
    TestBed.inject(AgentsStore).selected.set(task);
    fixture.detectChanges();
    expect(element.querySelector('.notice.danger')?.textContent).toContain('Check its effects manually');
    expect(element.querySelector('.chip.warning')?.textContent).toContain('unknown');
    const review = [...element.querySelectorAll('.notice.warning')].find((notice) =>
      notice.textContent?.includes('result of operation')
    );
    expect(review?.textContent).toContain('recovered:0');
    expect(review?.textContent).toContain('Check its effects manually');
    const output = actionOutput(element);
    expect(output?.open).toBe(true);
    expect(output?.textContent).toContain('may have completed before JARVIS stopped');
    expect(element.querySelector('.controls')?.textContent).toContain('Delete reviewed journal');
    expect(element.querySelector('.controls')?.textContent).not.toContain('Resume');
  });

  it('deletes a cancelled unknown journal only after explicit review confirmation', async () => {
    const { fixture, element } = await render();
    const task = agentTask('cancelled-unknown', 'cancelled');
    task.error = 'The cancelled action may have partly happened. Check its effects manually.';
    task.steps[0].outcome = 'unknown';
    task.steps[0].output = 'The action was interrupted before its result was recorded.';
    TestBed.inject(AgentsStore).selected.set(task);
    fixture.detectChanges();

    expect(element.querySelector('.detail-meta')?.textContent).toContain('Cancelled');
    expect(element.querySelector('.detail > .notice.warning')?.textContent).toContain('Check its effects manually');
    expect(element.querySelector('.step .notice.warning')?.textContent).toContain('cancelled-unknown:0');
    expect(actionOutput(element)?.open).toBe(true);
    expect(element.querySelector('.controls')?.textContent).not.toContain('Resume');

    api.deleteReviewedAgentTask.mockClear();
    button(element, 'Delete reviewed journal').click();
    await settle(fixture);
    expect(api.deleteReviewedAgentTask).not.toHaveBeenCalled();
    expect(element.querySelector('[role="alert"]')?.textContent).toContain("checked the action's actual effects");
    expect(element.querySelector('[role="alert"]')?.textContent).toContain('audit entries are retained');

    button(element, 'Keep journal').click();
    await settle(fixture);
    expect(api.deleteReviewedAgentTask).not.toHaveBeenCalled();
    button(element, 'Delete reviewed journal').click();
    await settle(fixture);
    button(element, 'Delete journal').click();
    await settle(fixture);
    expect(api.deleteReviewedAgentTask).toHaveBeenCalledOnce();
    expect(api.deleteReviewedAgentTask).toHaveBeenCalledWith('cancelled-unknown');
  });

  it('expands the explanation for a denied action', async () => {
    const { fixture, element } = await render();
    const task = agentTask('denied', 'running');
    task.steps[0].outcome = 'denied';
    task.steps[0].output = 'Not run: the action changed while waiting; request it again for a fresh approval check.';
    TestBed.inject(AgentsStore).selected.set(task);
    fixture.detectChanges();
    const output = actionOutput(element);
    expect(output?.open).toBe(true);
    expect(output?.textContent).toContain('action changed while waiting');
    expect(element.querySelector('.chip.warning')?.textContent).toContain('denied');
  });

  it('expands the output for a failed action', async () => {
    const { fixture, element } = await render();
    const task = agentTask('failed-action', 'failed');
    task.steps[0].outcome = 'failed';
    task.steps[0].output = 'The tool process exited before it could finish.';
    TestBed.inject(AgentsStore).selected.set(task);
    fixture.detectChanges();
    const output = actionOutput(element);
    expect(output?.open).toBe(true);
    expect(output?.textContent).toContain('tool process exited before it could finish');
    expect(element.querySelector('.chip.danger')?.textContent).toContain('failed');
  });

  it('shows a readable label for an action in progress', async () => {
    const { fixture, element } = await render();
    const task = agentTask('in-progress', 'running');
    task.steps[0].outcome = 'inProgress';
    TestBed.inject(AgentsStore).selected.set(task);
    fixture.detectChanges();
    expect(element.querySelector('.chip.warning')?.textContent?.trim()).toBe('In progress');
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

  it.each(['restore', 'automatic'] as const)(
    'keeps a disappeared explicit model selected until the user can %s',
    async (recovery) => {
      const { fixture, element } = await render();
      const models = TestBed.inject(ModelsStore);
      await models.refresh();
      const available: ProviderModels = {
        provider: { id: 'local', displayName: 'Local', kind: 'localServer', locality: 'onDevice' },
        models: [
          {
            id: 'coder',
            displayName: 'Coder',
            providerId: 'local',
            contextWindow: 4096,
            capabilities: { chat: true, streaming: true, embeddings: false, toolCalling: true, vision: false },
          },
        ],
        error: null,
      };
      models.providers.set([available]);
      await settle(fixture);
      const goal = element.querySelector<HTMLTextAreaElement>('textarea[name="goal"]')!;
      goal.value = 'Inspect the workspace';
      goal.dispatchEvent(new Event('input'));
      const selection = element.querySelector<HTMLSelectElement>('select[name="model"]')!;
      const key = optionKey('local', 'coder');
      selection.value = key;
      selection.dispatchEvent(new Event('change'));
      await settle(fixture);
      expect(button(element, 'Start task').disabled).toBe(false);
      api.startAgentTask.mockClear();

      models.providers.set([]);
      await settle(fixture);
      expect(selection.value).toBe(key);
      expect(selection.selectedOptions[0]?.disabled).toBe(true);
      expect(selection.selectedOptions[0]?.textContent).toContain('Selected model unavailable');
      expect(element.querySelector('.new-task [role="status"]')?.textContent).toContain(
        'Choose another model or select Automatic'
      );
      expect(button(element, 'Start task').disabled).toBe(true);
      element
        .querySelector<HTMLFormElement>('form.new-task')!
        .dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
      await settle(fixture);
      expect(api.startAgentTask).not.toHaveBeenCalled();
      expect(goal.value).toBe('Inspect the workspace');

      if (recovery === 'restore') {
        models.providers.set([available]);
      } else {
        selection.value = '';
        selection.dispatchEvent(new Event('change'));
      }
      await settle(fixture);
      expect(element.querySelector('.new-task [role="status"]')).toBeNull();
      expect(button(element, 'Start task').disabled).toBe(false);
      button(element, 'Start task').click();
      await settle(fixture);
      expect(api.startAgentTask).toHaveBeenCalledOnce();
      expect(api.startAgentTask).toHaveBeenCalledWith({
        goal: 'Inspect the workspace',
        providerId: recovery === 'restore' ? 'local' : null,
        model: recovery === 'restore' ? 'coder' : null,
        maxSteps: null,
        maxDurationSecs: null,
      });
    }
  );

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

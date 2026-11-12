import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import type { AgentStartRequest, AgentTask, TaskSummary } from '@jarvis/shared-types';

import { AgentsStore, MAX_GOAL_LENGTH, summarize } from '../../../core/state/agents.store';
import { ModelsStore, optionKey, type ModelOption } from '../../../core/state/models.store';
import { SettingsStore } from '../../../core/state/settings.store';
import { AGENT_STATUS, agentTask } from '../../../testing/agents.fixture';
import { TasksPanel } from './tasks-panel';

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((complete) => {
    resolve = complete;
  });
  return { promise, resolve };
}

function model(id: string): ModelOption {
  return {
    provider: { id: 'local-llama', displayName: 'Local', kind: 'localServer', locality: 'onDevice' },
    model: {
      id,
      displayName: id,
      providerId: 'local-llama',
      contextWindow: 4096,
      capabilities: { chat: true, streaming: true, embeddings: false, vision: false, toolCalling: true },
    },
  };
}

describe('TasksPanel', () => {
  async function render() {
    const store = {
      busy: signal(false),
      status: signal(AGENT_STATUS),
      selected: signal<AgentTask | null>(null),
      selectedId: signal<string | null>(null),
      selectionVersion: signal(0),
      tasks: signal<TaskSummary[]>([]),
      hasFinished: signal(false),
      clearFinished: vi.fn(async () => undefined),
      select: vi.fn(async (_id: string): Promise<void> => undefined),
      deleteReviewed: vi.fn(async (_id: string) => undefined),
      refreshTasks: vi.fn(async () => undefined),
      startTask: vi
        .fn<(request: AgentStartRequest, shouldSelect?: () => boolean) => Promise<AgentTask | null>>()
        .mockResolvedValue(agentTask('created', 'pending')),
    };
    const models = {
      chatOptions: signal([model('qwen'), model('coder')]),
      loading: signal(false),
      refresh: vi.fn(async () => undefined),
    };
    TestBed.configureTestingModule({
      providers: [
        provideRouter([]),
        { provide: AgentsStore, useValue: store },
        { provide: ModelsStore, useValue: models },
        { provide: SettingsStore, useValue: { settings: signal(null) } },
      ],
    });
    const fixture = TestBed.createComponent(TasksPanel);
    await fixture.whenStable();
    fixture.detectChanges();
    const element = fixture.nativeElement as HTMLElement;
    const form = element.querySelector<HTMLFormElement>('form.new-task')!;
    const goal = element.querySelector<HTMLTextAreaElement>('textarea[name="goal"]')!;
    const selector = element.querySelector<HTMLSelectElement>('select[name="model"]')!;
    const steps = element.querySelector<HTMLInputElement>('input[name="maxSteps"]')!;
    const minutes = element.querySelector<HTMLInputElement>('input[name="maxMinutes"]')!;
    const button = element.querySelector<HTMLButtonElement>('button[type="submit"]')!;
    const typeGoal = (value: string) => {
      goal.value = value;
      goal.dispatchEvent(new Event('input'));
      fixture.detectChanges();
    };
    const select = (value: string) => {
      selector.value = value;
      selector.dispatchEvent(new Event('change'));
      fixture.detectChanges();
    };
    const number = (field: HTMLInputElement, value: string) => {
      field.value = value;
      field.dispatchEvent(new Event('input'));
      fixture.detectChanges();
    };
    const submit = () => form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
    return { store, models, fixture, element, form, goal, selector, steps, minutes, button, typeGoal, select, number, submit };
  }

  it('submits frozen goal/options and clears an unchanged goal only after confirmation', async () => {
    const { store, fixture, goal, selector, steps, minutes, typeGoal, select, number, submit } = await render();
    const pending = deferred<AgentTask | null>();
    store.startTask.mockReturnValueOnce(pending.promise);
    typeGoal('  Inspect the workspace  ');
    select(optionKey('local-llama', 'qwen'));
    number(steps, '8');
    number(minutes, '2');
    submit();
    expect(store.startTask).toHaveBeenCalledExactlyOnceWith(
      { goal: 'Inspect the workspace', providerId: 'local-llama', model: 'qwen', maxSteps: 8, maxDurationSecs: 120 },
      expect.any(Function)
    );
    expect(store.startTask.mock.calls[0][1]!()).toBe(true);
    expect(goal.value).toBe('  Inspect the workspace  ');
    pending.resolve(agentTask('created', 'pending'));
    await fixture.whenStable();
    fixture.detectChanges();
    expect(goal.value).toBe('');
    expect(selector.value).toBe(optionKey('local-llama', 'qwen'));
    expect(steps.value).toBe('8');
    expect(minutes.value).toBe('2');
  });

  it('preserves newer goal and options and refuses automatic selection of the older task', async () => {
    const { store, fixture, goal, selector, steps, minutes, typeGoal, select, number, submit } = await render();
    const pending = deferred<AgentTask | null>();
    store.startTask.mockReturnValueOnce(pending.promise);
    typeGoal('Original goal');
    select(optionKey('local-llama', 'qwen'));
    number(steps, '8');
    number(minutes, '2');
    submit();
    typeGoal('Next task draft');
    select(optionKey('local-llama', 'coder'));
    number(steps, '10');
    number(minutes, '3');
    expect(store.startTask.mock.calls[0][1]!()).toBe(false);
    pending.resolve(agentTask('created', 'pending'));
    await fixture.whenStable();
    fixture.detectChanges();
    expect(goal.value).toBe('Next task draft');
    expect(selector.value).toBe(optionKey('local-llama', 'coder'));
    expect(steps.value).toBe('10');
    expect(minutes.value).toBe('3');
    expect(store.startTask.mock.calls[0][0]).toMatchObject({
      goal: 'Original goal',
      model: 'qwen',
      maxSteps: 8,
      maxDurationSecs: 120,
    });
  });

  it.each(['goal', 'model', 'steps', 'minutes'] as const)(
    'retains an edit away and back in %s during creation',
    async (field) => {
      const { store, fixture, goal, steps, minutes, typeGoal, select, number, submit } = await render();
      const pending = deferred<AgentTask | null>();
      store.startTask.mockReturnValueOnce(pending.promise);
      typeGoal('Original goal');
      select(optionKey('local-llama', 'qwen'));
      number(steps, '8');
      number(minutes, '2');
      submit();
      if (field === 'goal') {
        typeGoal('Changed');
        typeGoal('Original goal');
      } else if (field === 'model') {
        select(optionKey('local-llama', 'coder'));
        select(optionKey('local-llama', 'qwen'));
      } else if (field === 'steps') {
        number(steps, '10');
        number(steps, '8');
      } else {
        number(minutes, '3');
        number(minutes, '2');
      }
      expect(store.startTask.mock.calls[0][1]!()).toBe(false);
      pending.resolve(agentTask('created', 'pending'));
      await fixture.whenStable();
      fixture.detectChanges();
      expect(goal.value).toBe('Original goal');
      expect(store.startTask).toHaveBeenCalledOnce();
    }
  );

  it('coalesces forced duplicate submits while the store itself is not busy', async () => {
    const { store, fixture, button, typeGoal, submit } = await render();
    const pending = deferred<AgentTask | null>();
    store.startTask.mockReturnValueOnce(pending.promise);
    typeGoal('One request');
    submit();
    fixture.detectChanges();
    expect(button.disabled).toBe(true);
    submit();
    submit();
    expect(store.busy()).toBe(false);
    expect(store.startTask).toHaveBeenCalledOnce();
    pending.resolve(null);
    await fixture.whenStable();
    fixture.detectChanges();
    expect(button.disabled).toBe(false);
  });

  it('keeps a refused goal and permits another explicit submission', async () => {
    const { store, fixture, goal, typeGoal, submit } = await render();
    store.startTask.mockResolvedValueOnce(null);
    typeGoal('Keep this refused draft');
    submit();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(goal.value).toBe('Keep this refused draft');
    expect(store.startTask).toHaveBeenCalledOnce();
    submit();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(store.startTask).toHaveBeenCalledTimes(2);
    expect(goal.value).toBe('');
  });

  it('does not select or clear a new component draft after the original component closes', async () => {
    const { store, fixture, typeGoal, submit } = await render();
    const pending = deferred<AgentTask | null>();
    store.startTask.mockReturnValueOnce(pending.promise);
    typeGoal('Old submitted goal');
    submit();
    const shouldSelect = store.startTask.mock.calls[0][1]!;
    fixture.destroy();
    expect(shouldSelect()).toBe(false);
    const next = TestBed.createComponent(TasksPanel);
    await next.whenStable();
    next.detectChanges();
    const nextGoal = (next.nativeElement as HTMLElement).querySelector<HTMLTextAreaElement>('textarea[name="goal"]')!;
    nextGoal.value = 'New component draft';
    nextGoal.dispatchEvent(new Event('input'));
    pending.resolve(agentTask('created', 'pending'));
    await next.whenStable();
    next.detectChanges();
    expect(nextGoal.value).toBe('New component draft');
    expect(shouldSelect()).toBe(false);
    expect(store.startTask).toHaveBeenCalledOnce();
  });

  it('rejects invalid, disabled and unavailable submissions before dispatch', async () => {
    const { store, models, goal, fixture, typeGoal, select, submit } = await render();
    for (const value of [' ', 'x'.repeat(MAX_GOAL_LENGTH + 1)]) {
      typeGoal(value);
      submit();
    }
    typeGoal('Valid goal');
    select(optionKey('local-llama', 'qwen'));
    models.chatOptions.set([]);
    fixture.detectChanges();
    submit();
    expect(goal.value).toBe('Valid goal');
    expect(store.startTask).not.toHaveBeenCalled();
    expect(fixture.nativeElement.textContent).toContain('selected model is no longer available');
    models.chatOptions.set([model('qwen')]);
    store.status.set({ ...AGENT_STATUS, enabled: false });
    fixture.detectChanges();
    submit();
    expect(store.startTask).not.toHaveBeenCalled();
  });

  it('requires a fresh confirmation when the selected journal is replaced before rendering', async () => {
    const { fixture, element, store } = await render();
    const task = agentTask('reviewed', 'cancelled');
    task.steps[0].outcome = 'unknown';
    store.selected.set(task);
    store.selectedId.set(task.id);
    fixture.detectChanges();
    const findButton = (label: string) =>
      [...element.querySelectorAll('button')].find((button) => button.textContent?.trim() === label);

    findButton('Delete reviewed journal')!.click();
    fixture.detectChanges();
    const oldConfirmation = findButton('Delete journal')!;
    store.selected.set({ ...task, updatedAtMs: task.updatedAtMs + 1 });
    oldConfirmation.click();
    expect(store.deleteReviewed).not.toHaveBeenCalled();

    fixture.detectChanges();
    expect(findButton('Delete journal')).toBeUndefined();
    findButton('Delete reviewed journal')!.click();
    fixture.detectChanges();
    findButton('Delete journal')!.click();
    expect(store.deleteReviewed).toHaveBeenCalledExactlyOnceWith(task.id);
  });

  it('discards reviewed deletion confirmation when the user picks another task and returns', async () => {
    const { fixture, element, store } = await render();
    const reviewed = agentTask('reviewed', 'cancelled');
    reviewed.steps[0].outcome = 'unknown';
    const other = agentTask('other', 'completed');
    store.selected.set(reviewed);
    store.selectedId.set(reviewed.id);
    store.tasks.set([summarize(reviewed), summarize(other)]);
    store.select.mockImplementation(async (id) => {
      store.selectedId.set(id);
      store.selected.set(id === reviewed.id ? reviewed : other);
      store.selectionVersion.update((version) => version + 1);
    });
    fixture.detectChanges();
    const buttons = () => [...element.querySelectorAll('button')];
    buttons()
      .find((button) => button.textContent?.trim() === 'Delete reviewed journal')!
      .click();
    fixture.detectChanges();
    const oldConfirmation = buttons().find((button) => button.textContent?.trim() === 'Delete journal')!;

    const taskButtons = element.querySelectorAll<HTMLButtonElement>('.task-item');
    taskButtons[1].click();
    taskButtons[0].click();
    oldConfirmation.click();
    fixture.detectChanges();

    expect(store.deleteReviewed).not.toHaveBeenCalled();
    expect(buttons().some((button) => button.textContent?.trim() === 'Delete journal')).toBe(false);
    expect(buttons().some((button) => button.textContent?.trim() === 'Delete reviewed journal')).toBe(true);
  });

  it('drops a rendered confirmation when a new journal selection starts before rendering', async () => {
    const { fixture, element, store } = await render();
    const task = agentTask('reviewed', 'cancelled');
    task.steps[0].outcome = 'unknown';
    store.selected.set(task);
    store.selectedId.set(task.id);
    fixture.detectChanges();
    const findButton = (label: string) =>
      [...element.querySelectorAll('button')].find((button) => button.textContent?.trim() === label);
    const open = findButton('Delete reviewed journal')!;
    open.click();
    fixture.detectChanges();

    store.selectionVersion.update((version) => version + 1);
    findButton('Delete journal')!.click();
    open.click();
    fixture.detectChanges();

    expect(store.deleteReviewed).not.toHaveBeenCalled();
    expect(findButton('Delete journal')).toBeUndefined();
  });

  it('refreshes both the task list and open journal through the store', async () => {
    const { element, store } = await render();
    [...element.querySelectorAll('button')].find((button) => button.textContent?.trim() === 'Refresh tasks')!.click();
    expect(store.refreshTasks).toHaveBeenCalledOnce();
  });

  it('checks current eligibility and busy state before clearing finished journals', async () => {
    const { fixture, element, store } = await render();
    store.hasFinished.set(true);
    fixture.detectChanges();
    const clear = [...element.querySelectorAll('button')].find((button) => button.textContent?.trim() === 'Clear finished')!;

    store.hasFinished.set(false);
    clear.click();
    store.hasFinished.set(true);
    store.busy.set(true);
    clear.click();
    expect(store.clearFinished).not.toHaveBeenCalled();

    store.busy.set(false);
    clear.click();
    expect(store.clearFinished).toHaveBeenCalledOnce();
  });
});

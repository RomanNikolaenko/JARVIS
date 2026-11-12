import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideRouter, Router } from '@angular/router';
import type { AgentStartRequest, AgentTask } from '@jarvis/shared-types';

import { IpcError } from '../../core/ipc/ipc-error';
import { AgentsStore, MAX_GOAL_LENGTH } from '../../core/state/agents.store';
import { agentTask } from '../../testing/agents.fixture';
import { ChatAgentAction } from './chat-agent-action';

describe('Chat agent action', () => {
  function render() {
    const agents = {
      startTask: vi
        .fn<(request: AgentStartRequest, shouldSelect?: () => boolean) => Promise<AgentTask | null>>()
        .mockResolvedValue(agentTask('new-task', 'pending')),
      error: signal<IpcError | null>(null),
    };
    TestBed.configureTestingModule({ providers: [provideRouter([]), { provide: AgentsStore, useValue: agents }] });
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    const fixture = TestBed.createComponent(ChatAgentAction);
    fixture.componentRef.setInput('text', '  Inspect the workspace  ');
    const started = vi.fn();
    fixture.componentInstance.started.subscribe(started);
    fixture.detectChanges();
    const element = fixture.nativeElement as HTMLElement;
    const button = element.querySelector('button')!;
    return { agents, navigate, fixture, element, button, started };
  }

  it('submits one task with the selected model and default limits, then opens its journal', async () => {
    const { agents, navigate, fixture, button, started } = render();
    fixture.componentRef.setInput('model', { providerId: 'local', model: 'coder' });
    let resolve!: (task: AgentTask) => void;
    agents.startTask.mockReturnValueOnce(
      new Promise((done) => {
        resolve = done;
      })
    );
    fixture.detectChanges();
    button.click();
    button.click();
    expect(agents.startTask).toHaveBeenCalledTimes(1);
    expect(agents.startTask).toHaveBeenCalledWith(
      {
        goal: 'Inspect the workspace',
        providerId: 'local',
        model: 'coder',
        maxSteps: null,
        maxDurationSecs: null,
      },
      expect.any(Function)
    );
    resolve(agentTask('new-task', 'pending'));
    await vi.waitFor(() => expect(navigate).toHaveBeenCalledWith(['/agents'], { queryParams: { task: 'new-task' } }));
    expect(started).toHaveBeenCalledWith({
      text: '  Inspect the workspace  ',
      context: 0,
      revision: 0,
      model: { providerId: 'local', model: 'coder' },
    });
  });

  it('keeps the draft on a backend refusal and shows the error', async () => {
    const { agents, navigate, fixture, element, button, started } = render();
    agents.error.set(new IpcError('permissionDenied', 'Cloud agents are disabled'));
    agents.startTask.mockResolvedValue(null);
    button.click();
    await vi.waitFor(() => {
      fixture.detectChanges();
      expect(element.textContent).toContain('Cloud agents are disabled');
    });
    expect(started).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
  });

  it('blocks empty, oversized and disabled requests', () => {
    const { fixture, button, agents } = render();
    for (const text of ['', '  ', 'a'.repeat(MAX_GOAL_LENGTH + 1)]) {
      fixture.componentRef.setInput('text', text);
      fixture.detectChanges();
      expect(button.disabled).toBe(true);
      button.click();
    }
    fixture.componentRef.setInput('text', 'valid');
    fixture.componentRef.setInput('disabled', true);
    fixture.detectChanges();
    button.click();
    expect(agents.startTask).not.toHaveBeenCalled();
  });

  it('retains a link to a created task if navigation fails, and supports automatic routing', async () => {
    const { fixture, navigate, agents, button, element, started } = render();
    navigate.mockRejectedValue(new Error('Navigation failed'));
    button.click();
    await vi.waitFor(() => {
      fixture.detectChanges();
      expect(element.textContent).toContain('Navigation failed');
    });
    expect(element.querySelector('a')?.getAttribute('href')).toBe('/agents?task=new-task');
    expect(started).toHaveBeenCalledTimes(1);
    expect(agents.startTask).toHaveBeenCalledWith(
      expect.objectContaining({ providerId: null, model: null }),
      expect.any(Function)
    );
  });

  it.each(['context', 'revision', 'model', 'text'] as const)('keeps a late confirmation out of a changed %s', async (change) => {
    const { agents, fixture, button, started, navigate, element } = render();
    let context = 1;
    let revision = 1;
    fixture.componentRef.setInput('readContext', () => context);
    fixture.componentRef.setInput('readRevision', () => revision);
    let finish!: (task: AgentTask) => void;
    agents.startTask.mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      })
    );
    fixture.detectChanges();
    button.click();
    const maySelect = agents.startTask.mock.calls[0][1]!;
    expect(maySelect()).toBe(true);
    if (change === 'context') ++context;
    else if (change === 'revision') ++revision;
    else if (change === 'model') fixture.componentRef.setInput('model', { providerId: 'next', model: 'next-model' });
    else fixture.componentRef.setInput('text', 'A newer goal');
    expect(maySelect()).toBe(false);
    finish(agentTask('created-for-old-draft', 'pending'));
    await fixture.whenStable();
    fixture.detectChanges();
    expect(started).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
    expect(element.querySelector('a')).toBeNull();
    expect(button.disabled).toBe(false);
    expect(agents.startTask).toHaveBeenCalledOnce();
  });

  it('does not dispatch a stale rendered request when the live origin refuses it', () => {
    const { agents, fixture, button } = render();
    fixture.componentRef.setInput('isCurrent', () => false);
    button.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    expect(agents.startTask).not.toHaveBeenCalled();
  });

  it('does not emit or navigate after the action component is destroyed', async () => {
    const { agents, fixture, button, started, navigate } = render();
    const busy = vi.fn();
    fixture.componentInstance.busyChange.subscribe(busy);
    let finish!: (task: AgentTask) => void;
    agents.startTask.mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      })
    );
    button.click();
    const pending = agents.startTask.mock.results[0].value;
    fixture.destroy();
    finish(agentTask('created-after-close', 'pending'));
    await pending;
    await Promise.resolve();
    expect(started).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
    expect(busy.mock.calls).toEqual([[true]]);
  });

  it('ignores a navigation failure after the source context changes', async () => {
    const { fixture, button, navigate, element } = render();
    let context = 1;
    fixture.componentRef.setInput('readContext', () => context);
    let reject!: (error: unknown) => void;
    navigate.mockReturnValueOnce(
      new Promise((_resolve, fail) => {
        reject = fail;
      })
    );
    fixture.detectChanges();
    button.click();
    await vi.waitFor(() => expect(navigate).toHaveBeenCalledOnce());
    ++context;
    reject(new Error('Obsolete navigation failure'));
    await fixture.whenStable();
    fixture.detectChanges();
    expect(element.textContent).not.toContain('Obsolete navigation failure');
  });
});

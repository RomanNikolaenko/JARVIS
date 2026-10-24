import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideRouter, Router } from '@angular/router';
import type { AgentTask } from '@jarvis/shared-types';

import { IpcError } from '../../core/ipc/ipc-error';
import { AgentsStore, MAX_GOAL_LENGTH } from '../../core/state/agents.store';
import { agentTask } from '../../testing/agents.fixture';
import { ChatAgentAction } from './chat-agent-action';

describe('Chat agent action', () => {
  function render() {
    const agents = {
      startTask: vi.fn<() => Promise<AgentTask | null>>().mockResolvedValue(agentTask('new-task', 'pending')),
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
    expect(agents.startTask).toHaveBeenCalledWith({
      goal: 'Inspect the workspace',
      providerId: 'local',
      model: 'coder',
      maxSteps: null,
      maxDurationSecs: null,
    });
    resolve(agentTask('new-task', 'pending'));
    await vi.waitFor(() => expect(navigate).toHaveBeenCalledWith(['/agents'], { queryParams: { task: 'new-task' } }));
    expect(started).toHaveBeenCalledWith('  Inspect the workspace  ');
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
    expect(agents.startTask).toHaveBeenCalledWith(expect.objectContaining({ providerId: null, model: null }));
  });
});

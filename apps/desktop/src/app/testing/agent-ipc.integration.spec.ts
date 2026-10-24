import { TestBed } from '@angular/core/testing';
import { provideRouter, Router } from '@angular/router';
import { emit } from '@tauri-apps/api/event';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import type { InvokeArgs } from '@tauri-apps/api/core';

import { AGENT_TASK_EVENT, APPROVALS_EVENT } from '../core/ipc/jarvis-api';
import { AgentsStore, summarize } from '../core/state/agents.store';
import { ChatAgentAction } from '../features/chat/chat-agent-action';
import { ApprovalDialog } from '../shared/approval-dialog/approval-dialog';
import { agentTask, approval } from './agents.fixture';

/** Uses the real facade, bridge and Tauri SDK; only the native transport is mocked. */
describe('Agent IPC integration', () => {
  afterEach(() => {
    // Destroy stores while the transport still exists so unlisten can run.
    TestBed.resetTestingModule();
    clearMocks();
    const globals = window as unknown as Record<string, unknown>;
    delete globals['__TAURI_INTERNALS__'];
    delete globals['__TAURI_EVENT_PLUGIN_INTERNALS__'];
  });

  it.each([true, false])('runs chat → task → approval → result (approved=%s)', async (approved) => {
    const command = vi.fn(async (name: string, args?: InvokeArgs) => {
      switch (name) {
        case 'list_pending_approvals':
          return [];
        case 'start_agent_task':
          // The native task can advance before its start command returns.
          await emit(AGENT_TASK_EVENT, agentTask('created', 'running', 10));
          return agentTask('created', 'pending', 10);
        case 'resolve_approval': {
          expect(args).toEqual({ id: 'permission', approved });
          await emit(APPROVALS_EVENT, []);
          await emit(AGENT_TASK_EVENT, {
            ...agentTask('created', 'completed', 30),
            result: approved ? 'File written' : 'Action denied; no file written',
          });
          return true;
        }
        default:
          throw new Error(`Unexpected command: ${name}`);
      }
    });
    mockIPC(command, { shouldMockEvents: true });
    TestBed.configureTestingModule({ providers: [provideRouter([])] });
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    const store = TestBed.inject(AgentsStore);
    await store.start();
    const composer = TestBed.createComponent(ChatAgentAction);
    composer.componentRef.setInput('text', '  Write the report  ');
    composer.componentRef.setInput('model', { providerId: 'local', model: 'coder' });
    composer.detectChanges();
    (composer.nativeElement as HTMLElement).querySelector('button')!.click();
    await vi.waitFor(() => expect(navigate).toHaveBeenCalledWith(['/agents'], { queryParams: { task: 'created' } }));
    expect(command).toHaveBeenCalledWith('start_agent_task', {
      request: {
        goal: 'Write the report',
        providerId: 'local',
        model: 'coder',
        maxSteps: null,
        maxDurationSecs: null,
      },
    });
    expect(store.selected()?.status).toBe('running');

    const dialog = TestBed.createComponent(ApprovalDialog);
    await emit(AGENT_TASK_EVENT, agentTask('created', 'waitingForApproval', 20));
    await emit(APPROVALS_EVENT, [approval('permission', 'medium', 'created')]);
    dialog.detectChanges();
    const element = dialog.nativeElement as HTMLElement;
    expect(element.querySelector('a')?.getAttribute('href')).toBe('/agents?task=created');
    const label = approved ? 'Allow once' : 'Deny';
    [...element.querySelectorAll('button')].find((button) => button.textContent?.includes(label))!.click();
    await vi.waitFor(() => expect(store.selected()?.status).toBe('completed'));
    dialog.detectChanges();
    expect(store.selected()?.result).toBe(approved ? 'File written' : 'Action denied; no file written');
    expect(store.approvals()).toEqual([]);
    expect(element.querySelector('[role="alertdialog"]')).toBeNull();
  });

  it('normalizes a native failure and recovers the selected journal through real command bindings', async () => {
    let fail = true;
    let current = agentTask('saved', 'running', 1);
    const command = vi.fn((name: string, args?: InvokeArgs) => {
      switch (name) {
        case 'list_pending_approvals':
          if (fail) throw { code: 'unavailable', message: 'Transport unavailable' };
          return [];
        case 'get_agent_task':
          expect(args).toEqual({ id: 'saved' });
          return current;
        case 'list_agent_tasks':
          expect(args).toEqual({ limit: 100 });
          return [summarize(current)];
        default:
          throw new Error(`Unexpected command: ${name}`);
      }
    });
    mockIPC(command, { shouldMockEvents: true });
    TestBed.configureTestingModule({});
    const store = TestBed.inject(AgentsStore);
    await store.select('saved');
    await store.start();
    expect(store.subscriptionError()?.code).toBe('unavailable');
    fail = false;
    current = agentTask('saved', 'completed', 50);
    await store.start();
    expect(store.subscriptionError()).toBeNull();
    expect(store.selected()?.status).toBe('completed');
    expect(store.tasks()[0].updatedAtMs).toBe(50);
  });
});

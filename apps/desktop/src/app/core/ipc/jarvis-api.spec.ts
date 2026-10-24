import { TestBed } from '@angular/core/testing';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';

import { IpcError } from './ipc-error';
import { JarvisApi } from './jarvis-api';

describe('JarvisApi', () => {
  let api: JarvisApi;

  beforeEach(() => {
    TestBed.configureTestingModule({});
    api = TestBed.inject(JarvisApi);
  });

  // clearMocks() leaves an empty `__TAURI_INTERNALS__` behind; remove the
  // globals so later test files see a plain browser again.
  afterEach(() => {
    clearMocks();
    const globals = window as unknown as Record<string, unknown>;
    delete globals['__TAURI_INTERNALS__'];
    delete globals['__TAURI_EVENT_PLUGIN_INTERNALS__'];
  });

  it('reports unavailable outside the desktop shell', async () => {
    expect(api.connected).toBe(false);
    await expect(api.getHealth()).rejects.toMatchObject({ code: 'unavailable' });
  });

  it('treats a leftover Tauri global without invoke as unavailable', async () => {
    mockIPC(() => null);
    clearMocks();

    expect(api.connected).toBe(false);
    await expect(api.getHealth()).rejects.toMatchObject({ code: 'unavailable' });
  });

  it('invokes commands with the argument names expected by Rust', async () => {
    const calls: { cmd: string; args: unknown }[] = [];
    mockIPC((cmd, args) => {
      calls.push({ cmd, args });
      if (cmd === 'get_recent_events') {
        return [];
      }
      if (cmd === 'cancel_chat') {
        return true;
      }
      return null;
    });

    expect(api.connected).toBe(true);
    await api.getRecentEvents(25);
    await api.cancelChat('req-1');
    await api.reportUiError({ message: 'boom', context: 'test' });

    expect(calls).toEqual([
      { cmd: 'get_recent_events', args: { limit: 25 } },
      { cmd: 'cancel_chat', args: { requestId: 'req-1' } },
      { cmd: 'report_ui_error', args: { report: { message: 'boom', context: 'test' } } },
    ]);
  });

  it('passes agent command arguments with the names expected by Rust', async () => {
    const calls: { cmd: string; args: unknown }[] = [];
    mockIPC((cmd, args) => {
      calls.push({ cmd, args });
      return cmd === 'get_audit_log' ? [] : true;
    });

    await api.resolveApproval('a-1', false);
    await api.getAuditLog(50, '12345678-1234-4234-8234-123456789012');
    await api.pauseAgentTask('t-1');
    await api.startAgentTask({
      goal: 'g',
      providerId: null,
      model: null,
      maxSteps: 3,
      maxDurationSecs: null,
    });

    expect(calls).toEqual([
      { cmd: 'resolve_approval', args: { id: 'a-1', approved: false } },
      { cmd: 'get_audit_log', args: { limit: 50, beforeId: '12345678-1234-4234-8234-123456789012' } },
      { cmd: 'pause_agent_task', args: { id: 't-1' } },
      {
        cmd: 'start_agent_task',
        args: {
          request: { goal: 'g', providerId: null, model: null, maxSteps: 3, maxDurationSecs: null },
        },
      },
    ]);
  });

  it('sends the full exclusive history cursor over IPC', async () => {
    const calls: { cmd: string; args: unknown }[] = [];
    mockIPC((cmd, args) => {
      calls.push({ cmd, args });
      return [];
    });
    const before = { occurredAtMs: 1234, id: '00000000-0000-0000-0000-000000000001' };
    await api.listEpisodes(100, null);
    await api.listEpisodes(100, before);
    expect(calls).toEqual([
      { cmd: 'list_episodes', args: { limit: 100, before: null } },
      { cmd: 'list_episodes', args: { limit: 100, before } },
    ]);
  });

  it('converts core error payloads into IpcError', async () => {
    mockIPC(() => {
      throw { code: 'permissionDenied', message: 'cloud disabled' };
    });

    const error = await api.getSettings().catch((e: unknown) => e);
    expect(error).toBeInstanceOf(IpcError);
    expect((error as IpcError).code).toBe('permissionDenied');
  });
});

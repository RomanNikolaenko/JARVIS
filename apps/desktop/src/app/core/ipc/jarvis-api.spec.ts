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

  afterEach(() => clearMocks());

  it('reports unavailable outside the desktop shell', async () => {
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

  it('converts core error payloads into IpcError', async () => {
    mockIPC(() => {
      throw { code: 'permissionDenied', message: 'cloud disabled' };
    });

    const error = await api.getSettings().catch((e: unknown) => e);
    expect(error).toBeInstanceOf(IpcError);
    expect((error as IpcError).code).toBe('permissionDenied');
  });
});

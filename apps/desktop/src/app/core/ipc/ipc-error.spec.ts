import { IpcError, isErrorPayload } from './ipc-error';

describe('IpcError', () => {
  it('maps structured core errors by code', () => {
    const error = IpcError.from({ code: 'validation', message: 'bad input' });
    expect(error).toBeInstanceOf(IpcError);
    expect(error.code).toBe('validation');
    expect(error.message).toBe('bad input');
  });

  it('treats Tauri string errors as internal', () => {
    const error = IpcError.from('Command foo not allowed by ACL');
    expect(error.code).toBe('internal');
    expect(error.message).toContain('not allowed');
  });

  it('wraps plain errors and unknown values', () => {
    expect(IpcError.from(new Error('boom')).message).toBe('boom');
    expect(IpcError.from(42).message).toBe('Unknown error');
  });

  it('returns IpcError instances unchanged', () => {
    const original = new IpcError('timeout', 'slow');
    expect(IpcError.from(original)).toBe(original);
  });

  it('rejects payloads with unknown codes', () => {
    expect(isErrorPayload({ code: 'teapot', message: 'x' })).toBe(false);
    expect(isErrorPayload({ code: 'io' })).toBe(false);
    expect(isErrorPayload(null)).toBe(false);
    expect(isErrorPayload({ code: 'io', message: 'disk' })).toBe(true);
  });
});

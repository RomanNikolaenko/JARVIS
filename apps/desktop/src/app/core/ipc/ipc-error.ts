import type { ErrorCode, ErrorPayload } from '@jarvis/shared-types';

/** Error raised by any call into the Rust core. UI code branches on `code`. */
export class IpcError extends Error {
  constructor(
    readonly code: ErrorCode,
    message: string,
    /** Preserve native refusals and guards that ran before dispatch. */
    readonly origin: 'local' | 'core' | 'transport' | 'notDispatched' = 'local'
  ) {
    super(message);
    this.name = 'IpcError';
  }

  /** Normalizes anything thrown by Tauri's `invoke` into an `IpcError`. */
  static from(error: unknown): IpcError {
    if (error instanceof IpcError) {
      return error;
    }
    if (isErrorPayload(error)) {
      return new IpcError(error.code, error.message, 'core');
    }
    // Tauri reports argument deserialization and permission failures as strings.
    if (typeof error === 'string') {
      return new IpcError('internal', error, 'transport');
    }
    if (error instanceof Error) {
      return new IpcError('internal', error.message, 'transport');
    }
    return new IpcError('internal', 'Unknown error', 'transport');
  }
}

const ERROR_CODES: ReadonlySet<string> = new Set<ErrorCode>([
  'validation',
  'notFound',
  'permissionDenied',
  'privacyBlocked',
  'unsupported',
  'timeout',
  'cancelled',
  'unavailable',
  'limitExceeded',
  'config',
  'io',
  'internal',
]);

export function isErrorPayload(value: unknown): value is ErrorPayload {
  if (typeof value !== 'object' || value === null) {
    return false;
  }
  const candidate = value as Record<string, unknown>;
  return typeof candidate['code'] === 'string' && ERROR_CODES.has(candidate['code']) && typeof candidate['message'] === 'string';
}

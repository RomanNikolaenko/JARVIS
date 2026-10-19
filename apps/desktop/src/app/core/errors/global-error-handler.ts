import { ErrorHandler, Injectable, inject } from '@angular/core';

import { JarvisApi } from '../ipc/jarvis-api';

const MIN_REPORT_INTERVAL_MS = 2_000;

/**
 * Logs unhandled UI errors to the console and forwards them to the core so
 * they appear in the log files and the system event log. Reports are
 * throttled here and rate limited again in the core.
 */
@Injectable()
export class GlobalErrorHandler implements ErrorHandler {
  private readonly api = inject(JarvisApi);
  private lastReportAt = 0;

  handleError(error: unknown): void {
    console.error(error);

    const now = Date.now();
    if (!this.api.connected || now - this.lastReportAt < MIN_REPORT_INTERVAL_MS) {
      return;
    }
    this.lastReportAt = now;

    const message = error instanceof Error ? `${error.name}: ${error.message}` : String(error);
    this.api
      .reportUiError({ message, context: 'angular' })
      .catch((reportError: unknown) => console.warn('Failed to report UI error', reportError));
  }
}

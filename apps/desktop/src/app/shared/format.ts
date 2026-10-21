import type { DownloadStatus } from '@jarvis/shared-types';

const BYTE_UNITS = ['B', 'KB', 'MB', 'GB', 'TB'] as const;

export function formatBytes(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined || !Number.isFinite(bytes) || bytes < 0) {
    return '—';
  }
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < BYTE_UNITS.length - 1) {
    value /= 1024;
    unit++;
  }
  const digits = unit === 0 || value >= 100 ? 0 : 1;
  return `${value.toFixed(digits)} ${BYTE_UNITS[unit]}`;
}

export function formatPercent(value: number | null | undefined): string {
  if (value === null || value === undefined || !Number.isFinite(value)) {
    return '—';
  }
  return `${Math.round(value)}%`;
}

export function formatUptime(totalSeconds: number | null | undefined): string {
  if (totalSeconds === null || totalSeconds === undefined || totalSeconds < 0) {
    return '—';
  }
  const days = Math.floor(totalSeconds / 86_400);
  const hours = Math.floor((totalSeconds % 86_400) / 3_600);
  const minutes = Math.floor((totalSeconds % 3_600) / 60);
  if (days > 0) {
    return `${days}d ${hours}h`;
  }
  if (hours > 0) {
    return `${hours}h ${minutes}m`;
  }
  return `${minutes}m ${Math.floor(totalSeconds % 60)}s`;
}

export function formatClock(timestampMs: number): string {
  return new Date(timestampMs).toLocaleTimeString(undefined, {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
    hour12: false,
  });
}

/** Token counts: `850`, `3.2k`. */
export function formatTokens(tokens: number): string {
  return tokens >= 1000 ? `${(tokens / 1000).toFixed(1)}k` : `${tokens}`;
}

/** Time for today, otherwise the date (local time). */
export function formatShortDate(timestampMs: number, now = Date.now()): string {
  const date = new Date(timestampMs);
  const today = new Date(now);
  if (date.toDateString() === today.toDateString()) {
    return date.toLocaleTimeString(undefined, {
      hour: '2-digit',
      minute: '2-digit',
      hour12: false,
    });
  }
  return date.toLocaleDateString(undefined, {
    day: 'numeric',
    month: 'short',
    year: date.getFullYear() === today.getFullYear() ? undefined : 'numeric',
  });
}

/** Progress of a download in percent (0 while the size is unknown). */
export function downloadPercent(download: DownloadStatus): number {
  return download.totalBytes ? Math.min(100, Math.round((download.receivedBytes / download.totalBytes) * 100)) : 0;
}

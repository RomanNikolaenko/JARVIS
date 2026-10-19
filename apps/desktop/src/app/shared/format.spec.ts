import { formatBytes, formatPercent, formatUptime } from './format';

describe('format helpers', () => {
  it('formats byte sizes with binary units', () => {
    expect(formatBytes(512)).toBe('512 B');
    expect(formatBytes(1536)).toBe('1.5 KB');
    expect(formatBytes(16 * 1024 ** 3)).toBe('16.0 GB');
    expect(formatBytes(null)).toBe('—');
    expect(formatBytes(-1)).toBe('—');
  });

  it('rounds percentages', () => {
    expect(formatPercent(42.6)).toBe('43%');
    expect(formatPercent(undefined)).toBe('—');
  });

  it('formats uptime compactly', () => {
    expect(formatUptime(42)).toBe('0m 42s');
    expect(formatUptime(3_720)).toBe('1h 2m');
    expect(formatUptime(90_000)).toBe('1d 1h');
  });
});

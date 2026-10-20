import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import type { HealthReport, SystemMetrics } from '@jarvis/shared-types';

import { JarvisApi } from '../../core/ipc/jarvis-api';
import { SystemStore } from '../../core/state/system.store';
import { Dashboard } from './dashboard';

const REPORT: HealthReport = {
  status: 'degraded',
  version: '0.1.0',
  uptimeSecs: 125,
  checkedAtMs: 0,
  components: [
    {
      id: 'config',
      label: 'Configuration',
      status: 'healthy',
      message: 'Settings loaded',
      latencyMs: 0,
    },
    {
      id: 'ai',
      label: 'AI providers',
      status: 'degraded',
      message: 'Only the diagnostic echo provider is available',
      latencyMs: 1,
    },
  ],
};

const METRICS: SystemMetrics = {
  cpuUsagePercent: 12.4,
  memoryUsedBytes: 8 * 1024 ** 3,
  memoryTotalBytes: 16 * 1024 ** 3,
  processMemoryBytes: 120 * 1024 ** 2,
  processCpuPercent: 0.5,
  uptimeSecs: 3_600,
  sampledAtMs: 0,
};

describe('Dashboard', () => {
  beforeEach(async () => {
    const api: Partial<JarvisApi> = {
      getSystemMetrics: async () => METRICS,
    };
    await TestBed.configureTestingModule({
      imports: [Dashboard],
      providers: [provideRouter([]), { provide: JarvisApi, useValue: api }],
    }).compileComponents();
    TestBed.inject(SystemStore).health.set(REPORT);
  });

  it('shows the health headline and component statuses', async () => {
    const fixture = TestBed.createComponent(Dashboard);
    await fixture.whenStable();
    const element = fixture.nativeElement as HTMLElement;

    expect(element.querySelector('.headline')?.textContent).toContain('Operating with limitations');
    const components = Array.from(element.querySelectorAll('.component-name')).map((e) => e.textContent?.trim());
    expect(components).toEqual(['Configuration', 'AI providers']);
    expect(element.querySelector('jv-ai-core')?.getAttribute('class')).toContain('state-degraded');
  });

  it('renders resource metrics', async () => {
    const fixture = TestBed.createComponent(Dashboard);
    await fixture.whenStable();
    fixture.detectChanges();
    const text = (fixture.nativeElement as HTMLElement).querySelector('.resources')?.textContent;
    expect(text).toContain('12%');
    expect(text).toContain('8.0 GB / 16.0 GB');
  });
});

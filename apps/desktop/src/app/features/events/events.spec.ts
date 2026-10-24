import { TestBed } from '@angular/core/testing';
import type { SystemEvent } from '@jarvis/shared-types';

import { JarvisApi } from '../../core/ipc/jarvis-api';
import { EventLogStore } from '../../core/state/event-log.store';
import { Events } from './events';

describe('Events', () => {
  it('shows connection progress and errors, then retries without showing an empty-log message', async () => {
    let reject!: (reason: unknown) => void;
    const api = {
      onSystemEvent: vi.fn(async () => () => undefined),
      getRecentEvents: vi.fn(async (): Promise<SystemEvent[]> => [
        {
          id: 1,
          timestampMs: 1000,
          level: 'info',
          category: 'lifecycle',
          message: 'Recovered event',
        },
      ]),
    };
    api.getRecentEvents.mockReturnValueOnce(
      new Promise((_resolve, fail) => {
        reject = fail;
      })
    );
    TestBed.configureTestingModule({ imports: [Events], providers: [{ provide: JarvisApi, useValue: api }] });
    const store = TestBed.inject(EventLogStore);
    const fixture = TestBed.createComponent(Events);
    const element = fixture.nativeElement as HTMLElement;
    const first = store.start();
    await vi.waitFor(() => expect(api.getRecentEvents).toHaveBeenCalled());
    fixture.detectChanges();
    expect(element.textContent).toContain('Connecting to the event log');
    expect(element.textContent).not.toContain('No events match');
    reject(new Error('History unavailable'));
    await first;
    fixture.detectChanges();
    expect(element.querySelector('[role="alert"]')?.textContent).toContain('History unavailable');
    expect(element.textContent).not.toContain('No events match');
    const retry = element.querySelector('button')!;
    retry.click();
    fixture.detectChanges();
    expect(retry.disabled).toBe(true);
    await store.start();
    fixture.detectChanges();
    expect(element.textContent).toContain('Recovered event');
    expect(element.querySelector('[role="alert"]')).toBeNull();
    expect(element.querySelector('button')).toBeNull();
  });
});

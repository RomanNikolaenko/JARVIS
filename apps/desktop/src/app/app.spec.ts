import { TestBed } from '@angular/core/testing';
import { signal } from '@angular/core';
import { provideRouter } from '@angular/router';

import { App } from './app';
import { IpcError } from './core/ipc/ipc-error';
import { AgentsStore } from './core/state/agents.store';
import { SystemStore } from './core/state/system.store';
import { EventLogStore } from './core/state/event-log.store';
import { ChatStore } from './core/state/chat.store';

describe('App shell', () => {
  beforeEach(async () => {
    await TestBed.configureTestingModule({
      imports: [App],
      providers: [provideRouter([])],
    }).compileComponents();
  });

  it('renders the main navigation', async () => {
    const fixture = TestBed.createComponent(App);
    await fixture.whenStable();
    const links = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll<HTMLAnchorElement>('.nav-link')).map((a) =>
      a.textContent?.trim()
    );
    expect(links).toEqual(['Dashboard', 'Chat', 'Models', 'Runtime', 'Agents', 'Memory', 'Plugins', 'Event log', 'Settings']);
  });

  it('pauses decorative animations while another window has the focus', async () => {
    const focused = vi.spyOn(document, 'hasFocus').mockReturnValue(true);
    try {
      const fixture = TestBed.createComponent(App);
      await fixture.whenStable();
      const host = fixture.nativeElement as HTMLElement;
      expect(host.classList).not.toContain('jv-background');

      focused.mockReturnValue(false);
      window.dispatchEvent(new Event('blur'));
      fixture.detectChanges();
      expect(host.classList).toContain('jv-background');

      focused.mockReturnValue(true);
      window.dispatchEvent(new Event('focus'));
      fixture.detectChanges();
      expect(host.classList).not.toContain('jv-background');
    } finally {
      focused.mockRestore();
    }
  });

  it('shows an offline banner when the core is unreachable', async () => {
    const fixture = TestBed.createComponent(App);
    await fixture.whenStable();
    fixture.detectChanges();
    const element = fixture.nativeElement as HTMLElement;
    expect(element.querySelector('.banner')?.textContent).toContain('not reachable');
    expect(element.querySelector('jv-status-badge')?.textContent).toContain('Core offline');
  });

  it('reconnects agent updates when a failed health probe becomes reachable again', async () => {
    vi.useFakeTimers();
    try {
      const offline = new IpcError('unavailable', 'Core offline');
      const connectionError = signal<IpcError | null>(offline);
      const system = {
        status: signal('degraded'),
        appInfo: signal(null),
        connectionError,
        loadStaticInfo: vi.fn(async () => undefined),
        refreshHealth: vi.fn(async () => undefined),
      };
      TestBed.overrideProvider(SystemStore, { useValue: system });
      const agents = TestBed.inject(AgentsStore);
      const start = vi.spyOn(agents, 'start').mockResolvedValue();
      const lost = vi.spyOn(agents, 'connectionLost').mockImplementation(() => undefined);
      const eventLog = TestBed.inject(EventLogStore);
      const startEvents = vi.spyOn(eventLog, 'start').mockResolvedValue();
      const lostEvents = vi.spyOn(eventLog, 'connectionLost').mockImplementation(() => undefined);
      const lostChat = vi.spyOn(TestBed.inject(ChatStore), 'connectionLost').mockImplementation(() => undefined);
      const fixture = TestBed.createComponent(App);
      await vi.advanceTimersByTimeAsync(0);
      expect(lost).toHaveBeenCalledWith(offline);
      expect(lostEvents).toHaveBeenCalledWith(offline);
      expect(lostChat).toHaveBeenCalledWith(offline);
      expect(start).toHaveBeenCalledTimes(1);
      connectionError.set(null);
      await vi.advanceTimersByTimeAsync(15_000);
      expect(start).toHaveBeenCalledTimes(2);
      expect(startEvents).toHaveBeenCalledTimes(2);
      expect(lost).toHaveBeenCalledTimes(1);
      // A degraded component is still a reachable core.
      expect(system.status()).toBe('degraded');
      fixture.destroy();
      await vi.advanceTimersByTimeAsync(15_000);
      expect(start).toHaveBeenCalledTimes(2);
      expect(startEvents).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });
});

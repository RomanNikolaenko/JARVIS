import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { App } from './app';

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
    const links = Array.from(
      (fixture.nativeElement as HTMLElement).querySelectorAll<HTMLAnchorElement>('.nav-link'),
    ).map((a) => a.textContent?.trim());
    expect(links).toEqual(['Dashboard', 'Chat', 'Models', 'Runtime', 'Event log', 'Settings']);
  });

  it('shows an offline banner when the core is unreachable', async () => {
    const fixture = TestBed.createComponent(App);
    await fixture.whenStable();
    fixture.detectChanges();
    const element = fixture.nativeElement as HTMLElement;
    expect(element.querySelector('.banner')?.textContent).toContain('not reachable');
    expect(element.querySelector('jv-status-badge')?.textContent).toContain('Core offline');
  });
});

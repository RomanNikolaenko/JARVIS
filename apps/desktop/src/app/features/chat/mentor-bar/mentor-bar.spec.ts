import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import type { MentorStatus } from '@jarvis/shared-types';

import { JarvisApi } from '../../../core/ipc/jarvis-api';
import { MENTOR_OFF, MentorStore } from '../../../core/state/mentor.store';
import { MentorBar } from './mentor-bar';

describe('MentorBar', () => {
  let status: ReturnType<typeof signal<MentorStatus>>;
  let mentor: Record<string, unknown>;
  let api: Record<string, ReturnType<typeof vi.fn>>;

  async function render() {
    const fixture = TestBed.createComponent(MentorBar);
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
    return fixture.nativeElement as HTMLElement;
  }

  beforeEach(() => {
    status = signal<MentorStatus>(MENTOR_OFF);
    mentor = {
      status,
      notice: signal<string | null>(null),
      held: signal<readonly string[]>([]),
      starting: signal(false),
      openStart: vi.fn(() => (mentor['starting'] as ReturnType<typeof signal<boolean>>).set(true)),
      closeStart: vi.fn(),
      begin: vi.fn(async () => true),
      pause: vi.fn(),
      resume: vi.fn(),
      end: vi.fn(),
      look: vi.fn(),
      stopTyping: vi.fn(),
    };
    api = {
      getSettings: vi.fn(async () => ({ tools: { workspaceFolders: ['D:/work', 'D:/lab'] } })),
      addWorkspaceFolder: vi.fn(async () => ({ tools: { workspaceFolders: ['D:/work', 'D:/lab', 'D:/new'] } })),
    };
    TestBed.configureTestingModule({
      providers: [
        { provide: MentorStore, useValue: mentor },
        { provide: JarvisApi, useValue: api },
      ],
    });
  });

  it('start dialog lists workspace folders', async () => {
    const element = await render();
    const start = [...element.querySelectorAll('button')].find((b) => b.textContent?.includes('Start a lesson'));
    expect(start).toBeTruthy();
    start!.click();
    const fixture = TestBed.createComponent(MentorBar);
    (mentor['starting'] as ReturnType<typeof signal<boolean>>).set(true);
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
    const options = [...(fixture.nativeElement as HTMLElement).querySelectorAll('option')].map((o) => o.value);
    expect(options).toEqual(['D:/work', 'D:/lab']);
    const begin = [...(fixture.nativeElement as HTMLElement).querySelectorAll('button')].find(
      (b) => b.textContent?.trim() === 'Start'
    );
    begin!.click();
    expect(mentor['begin']).toHaveBeenCalledWith('D:/work');
  });

  it('bar shows listening, paused, typing and errors', async () => {
    status.set({ ...MENTOR_OFF, conversationId: 'c', projectFolder: 'D:/work/shop', phase: 'listening' });
    let element = await render();
    expect(element.textContent).toContain('Mentor is listening');
    expect(element.textContent).toContain('D:/work/shop');
    [...element.querySelectorAll('button')].find((b) => b.textContent?.includes('Pause'))!.click();
    expect(mentor['pause']).toHaveBeenCalled();

    status.set({ ...status(), phase: 'paused' });
    element = await render();
    expect(element.textContent).toContain('Paused');
    [...element.querySelectorAll('button')].find((b) => b.textContent?.includes('Resume'))!.click();
    expect(mentor['resume']).toHaveBeenCalled();

    status.set({ ...status(), phase: 'typing', typingFile: 'src/cart.ts', lastError: null });
    element = await render();
    expect(element.textContent).toContain('Typing into src/cart.ts');
    [...element.querySelectorAll('button')].find((b) => b.textContent?.includes('Stop typing'))!.click();
    expect(mentor['stopTyping']).toHaveBeenCalled();

    status.set({ ...status(), phase: 'listening', typingFile: null, lastError: 'The change was not written' });
    element = await render();
    expect(element.querySelector('[role=alert]')?.textContent).toContain('not written');
    [...element.querySelectorAll('button')].find((b) => b.textContent?.includes('End lesson'))!.click();
    expect(mentor['end']).toHaveBeenCalled();
  });
});

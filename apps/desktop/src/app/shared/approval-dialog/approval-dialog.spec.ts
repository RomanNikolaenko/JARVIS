import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { JarvisApi } from '../../core/ipc/jarvis-api';
import { AgentsStore } from '../../core/state/agents.store';
import { approval } from '../../testing/agents.fixture';
import { ApprovalDialog } from './approval-dialog';

describe('ApprovalDialog', () => {
  const api = { resolveApproval: vi.fn(async () => true) };

  async function render() {
    TestBed.configureTestingModule({
      imports: [ApprovalDialog],
      providers: [provideRouter([]), { provide: JarvisApi, useValue: api }],
    });
    const store = TestBed.inject(AgentsStore);
    const fixture = TestBed.createComponent(ApprovalDialog);
    fixture.detectChanges();
    return { fixture, store, element: fixture.nativeElement as HTMLElement };
  }

  function button(element: HTMLElement, text: string): HTMLButtonElement {
    const found = [...element.querySelectorAll('button')].find((b) =>
      b.textContent?.includes(text),
    );
    if (!found) {
      throw new Error(`no button "${text}"`);
    }
    return found;
  }

  beforeEach(() => api.resolveApproval.mockClear());

  it('stays hidden without pending approvals', async () => {
    const { element } = await render();
    expect(element.querySelector('[role="alertdialog"]')).toBeNull();
  });

  it('shows the oldest request with Deny focused', async () => {
    const { fixture, store, element } = await render();
    store.approvals.set([approval('a1', 'high'), approval('a2')]);
    fixture.detectChanges();
    await fixture.whenStable();

    const dialog = element.querySelector('[role="alertdialog"]');
    expect(dialog?.textContent).toContain('high action a1');
    expect(dialog?.textContent).toContain('delete_path');
    expect(dialog?.textContent).toContain('1 of 2 waiting');
    expect(element.querySelector('.warning')).not.toBeNull();
    expect(document.activeElement).toBe(button(element, 'Deny'));
  });

  it('sends the decision for the shown request', async () => {
    const { fixture, store, element } = await render();
    store.approvals.set([approval('a1'), approval('a2')]);
    fixture.detectChanges();

    button(element, 'Allow once').click();
    await fixture.whenStable();
    expect(api.resolveApproval).toHaveBeenCalledWith('a1', true);
    fixture.detectChanges();
    expect(element.textContent).toContain('medium action a2');

    button(element, 'Deny').click();
    await fixture.whenStable();
    expect(api.resolveApproval).toHaveBeenLastCalledWith('a2', false);
  });
});

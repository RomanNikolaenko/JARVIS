import {
  ChangeDetectionStrategy,
  Component,
  type ElementRef,
  afterNextRender,
  computed,
  inject,
  input,
  output,
  signal,
  viewChild,
} from '@angular/core';

import { MemoryStore } from '../../../core/state/memory.store';
import { Icon } from '../../../shared/icon/icon';

/**
 * Shows the recovery code once and encrypts memory after the user typed its
 * last group, which proves it was written down.
 */
@Component({
  selector: 'jv-encrypt-dialog',
  imports: [Icon],
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: {
    '(document:keydown.escape)': 'cancel()',
    '(keydown.tab)': 'trapFocus($event)',
    '(keydown.shift.tab)': 'trapFocus($event)',
  },
  template: `
    <div class="backdrop"></div>
    <div #dialog class="dialog" role="dialog" aria-modal="true" aria-labelledby="encrypt-title">
      <h2 id="encrypt-title">Encrypt memory</h2>
      <p>Write down this recovery code and keep it somewhere safe. It is shown only now.</p>
      <div class="code" role="group" aria-label="Recovery code">
        @for (group of groups(); track $index) {
          <span class="code-group">{{ group }}</span>
        }
      </div>
      <div class="row">
        <button type="button" class="ghost" (click)="copy()">
          <jv-icon name="check" [size]="14" />
          {{ copied() ? 'Copied' : 'Copy' }}
        </button>
        @if (copyFailed()) {
          <span class="dim">Copying failed: select the code and copy it.</span>
        }
      </div>
      <p class="warning">
        <jv-icon name="warning" [size]="14" />
        Without this code and without the keychain entry the memory cannot be recovered. JARVIS keeps the code in the system
        keychain; you need it if that entry is lost, for example after reinstalling the system.
      </p>
      <label class="field">
        <span class="field-label">Type the last group ({{ lastLength() }} digits)</span>
        <input
          #confirmation
          class="mono"
          aria-label="Last group of the recovery code"
          autocomplete="off"
          inputmode="numeric"
          spellcheck="false"
          [value]="typed()"
          (input)="typed.set(valueOf($event))" />
      </label>
      @if (store.dataError(); as error) {
        <p class="error-text" role="alert">{{ error.message }}</p>
      }
      <footer>
        <button type="button" class="ghost" [disabled]="encrypting()" (click)="cancel()">Cancel</button>
        <button type="button" class="primary" [disabled]="!matches() || encrypting()" (click)="encrypt()">
          <jv-icon name="shield" [size]="15" />
          {{ encrypting() ? 'Encrypting…' : 'Encrypt' }}
        </button>
      </footer>
    </div>
  `,
  styles: `
    .backdrop {
      position: fixed;
      inset: 0;
      z-index: 50;
      background: rgb(0 0 0 / 55%);
    }
    .dialog {
      position: fixed;
      top: 50%;
      left: 50%;
      z-index: 51;
      display: flex;
      flex-direction: column;
      gap: 10px;
      width: min(560px, calc(100vw - 32px));
      padding: 16px;
      border: 1px solid var(--jv-border-strong);
      border-radius: var(--jv-radius);
      background: var(--jv-bg-elevated);
      transform: translate(-50%, -50%);
    }
    h2 {
      margin: 0;
      font-size: 15px;
    }
    p {
      margin: 0;
      font-size: 12.5px;
    }
    .code {
      display: grid;
      grid-template-columns: repeat(4, auto);
      justify-content: start;
      gap: 6px 14px;
      padding: 10px 12px;
      border-radius: var(--jv-radius-sm);
      background: var(--jv-bg);
      font-family: var(--jv-font-mono);
      font-size: 16px;
      letter-spacing: 0.06em;
      user-select: all;
    }
    .row {
      display: flex;
      align-items: center;
      gap: 8px;
      font-size: 12.5px;
    }
    .warning {
      display: flex;
      gap: 6px;
      color: var(--jv-warning);
    }
    footer {
      display: flex;
      justify-content: flex-end;
      gap: 8px;
    }
  `,
})
export class EncryptDialog {
  protected readonly store = inject(MemoryStore);

  /** The recovery code: eight groups of six digits joined by `-`. */
  readonly code = input.required<string>();
  readonly closed = output<void>();

  protected readonly typed = signal('');
  protected readonly copied = signal(false);
  protected readonly copyFailed = signal(false);
  protected readonly groups = computed(() => this.code().split('-'));
  protected readonly lastLength = computed(() => this.groups().at(-1)?.length ?? 0);
  protected readonly matches = computed(() => this.typed().trim() === this.groups().at(-1));
  protected readonly encrypting = computed(() => this.store.dataOperation() === 'encrypt');
  private readonly dialog = viewChild.required<ElementRef<HTMLElement>>('dialog');
  private readonly confirmation = viewChild.required<ElementRef<HTMLInputElement>>('confirmation');

  constructor() {
    afterNextRender(() => this.confirmation().nativeElement.focus());
  }

  /** Keeps Tab inside the dialog while it is open. */
  protected trapFocus(event: Event): void {
    const focusable = [...this.dialog().nativeElement.querySelectorAll<HTMLElement>('button:not([disabled]), input')];
    if (!focusable.length) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    const backwards = (event as KeyboardEvent).shiftKey;
    if (backwards && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!backwards && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  }

  protected async copy(): Promise<void> {
    try {
      await navigator.clipboard.writeText(this.code());
      this.copied.set(true);
      this.copyFailed.set(false);
    } catch {
      this.copyFailed.set(true);
    }
  }

  protected async encrypt(): Promise<void> {
    if (!this.matches() || this.encrypting()) return;
    if (await this.store.encrypt(this.typed().trim())) {
      this.closed.emit();
    }
  }

  protected cancel(): void {
    if (!this.encrypting()) this.closed.emit();
  }

  protected valueOf(event: Event): string {
    return (event.target as HTMLInputElement).value;
  }
}

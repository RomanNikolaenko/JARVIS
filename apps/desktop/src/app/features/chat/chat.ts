import {
  ChangeDetectionStrategy,
  Component,
  type ElementRef,
  afterRenderEffect,
  computed,
  inject,
  signal,
  viewChild,
} from '@angular/core';

import { ChatStore, MAX_MESSAGE_LENGTH, type ModelOption } from '../../core/state/chat.store';
import { Icon } from '../../shared/icon/icon';

/** Distance from the bottom (px) within which new content keeps the view pinned. */
const STICKY_SCROLL_THRESHOLD = 80;

function optionKey(option: ModelOption): string {
  return `${option.provider.id}::${option.model.id}`;
}

@Component({
  selector: 'jv-chat',
  imports: [Icon],
  templateUrl: './chat.html',
  styleUrl: './chat.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Chat {
  protected readonly store = inject(ChatStore);
  private readonly scroller = viewChild<ElementRef<HTMLElement>>('scroller');
  private stickToBottom = true;

  protected readonly maxLength = MAX_MESSAGE_LENGTH;
  protected readonly draft = signal('');
  protected readonly optionKey = optionKey;
  protected readonly selectedKey = computed(() => {
    const selected = this.store.selected();
    return selected ? optionKey(selected) : '';
  });
  protected readonly canSend = computed(() => {
    const text = this.draft().trim();
    return text.length > 0 && text.length <= MAX_MESSAGE_LENGTH && !this.store.busy();
  });
  protected readonly nearLimit = computed(() => this.draft().length > MAX_MESSAGE_LENGTH * 0.8);

  constructor() {
    afterRenderEffect(() => {
      this.store.entries();
      const element = this.scroller()?.nativeElement;
      if (element && this.stickToBottom) {
        element.scrollTop = element.scrollHeight;
      }
    });
  }

  protected onScroll(element: HTMLElement): void {
    this.stickToBottom =
      element.scrollHeight - element.scrollTop - element.clientHeight < STICKY_SCROLL_THRESHOLD;
  }

  protected onInput(event: Event): void {
    this.draft.set((event.target as HTMLTextAreaElement).value);
  }

  protected onKeydown(event: KeyboardEvent): void {
    if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
      event.preventDefault();
      this.send();
    }
  }

  protected send(): void {
    if (!this.canSend()) {
      return;
    }
    const text = this.draft();
    this.draft.set('');
    this.stickToBottom = true;
    void this.store.send(text);
  }

  protected stop(): void {
    void this.store.cancel();
  }

  protected onModelChange(event: Event): void {
    const key = (event.target as HTMLSelectElement).value;
    const option = this.store.modelOptions().find((o) => optionKey(o) === key);
    if (option) {
      this.store.select(option);
    }
  }
}

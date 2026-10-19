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
import { RouterLink } from '@angular/router';

import { ChatStore, MAX_MESSAGE_LENGTH } from '../../core/state/chat.store';
import { type ModelOption, optionKey } from '../../core/state/models.store';
import { Icon } from '../../shared/icon/icon';
import { MarkdownPipe } from '../../shared/markdown/markdown';

/** Distance from the bottom (px) within which new content keeps the view pinned. */
const STICKY_SCROLL_THRESHOLD = 80;
const AUTOMATIC = 'auto';

interface OptionGroup {
  readonly label: string;
  readonly options: readonly ModelOption[];
}

@Component({
  selector: 'jv-chat',
  imports: [Icon, MarkdownPipe, RouterLink],
  templateUrl: './chat.html',
  styleUrl: './chat.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Chat {
  protected readonly store = inject(ChatStore);
  private readonly scroller = viewChild<ElementRef<HTMLElement>>('scroller');
  private stickToBottom = true;

  protected readonly automatic = AUTOMATIC;
  protected readonly maxLength = MAX_MESSAGE_LENGTH;
  protected readonly draft = signal('');
  protected readonly keyOf = (option: ModelOption): string =>
    optionKey(option.provider.id, option.model.id);

  protected readonly selectedKey = computed(() => {
    const selected = this.store.selected();
    return selected ? this.keyOf(selected) : AUTOMATIC;
  });
  protected readonly groups = computed<OptionGroup[]>(() => {
    const groups = new Map<string, { label: string; options: ModelOption[] }>();
    for (const option of this.store.modelOptions()) {
      const group = groups.get(option.provider.id) ?? {
        label: option.provider.displayName,
        options: [],
      };
      group.options.push(option);
      groups.set(option.provider.id, group);
    }
    return [...groups.values()];
  });
  protected readonly automaticLabel = computed(() => {
    const route = this.store.automaticRoute();
    return route?.model ? `Automatic — ${route.model}` : 'Automatic';
  });
  protected readonly canSend = computed(() => {
    const text = this.draft().trim();
    return text.length > 0 && text.length <= MAX_MESSAGE_LENGTH && !this.store.busy();
  });
  protected readonly nearLimit = computed(() => this.draft().length > MAX_MESSAGE_LENGTH * 0.8);

  constructor() {
    void this.store.loadModels();
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

  protected refreshModels(): void {
    void this.store.loadModels(true);
  }

  protected onModelChange(event: Event): void {
    const key = (event.target as HTMLSelectElement).value;
    this.store.select(this.store.modelOptions().find((o) => this.keyOf(o) === key) ?? null);
  }
}

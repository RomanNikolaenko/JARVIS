import {
  ChangeDetectionStrategy,
  Component,
  type ElementRef,
  afterRenderEffect,
  computed,
  effect,
  inject,
  signal,
  viewChild,
} from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { ActivatedRoute, Router, RouterLink } from '@angular/router';
import type { ContextReport } from '@jarvis/shared-types';

import { type ChatEntry, ChatStore, MAX_MESSAGE_LENGTH } from '../../core/state/chat.store';
import { MAX_FACT_LENGTH, MemoryStore } from '../../core/state/memory.store';
import { type ModelOption, optionKey } from '../../core/state/models.store';
import { formatTokens } from '../../shared/format';
import { Icon } from '../../shared/icon/icon';
import { MarkdownPipe } from '../../shared/markdown/markdown';
import { ConversationList } from './conversation-list/conversation-list';

/** Distance from the bottom (px) within which new content keeps the view pinned. */
const STICKY_SCROLL_THRESHOLD = 80;
const AUTOMATIC = 'auto';
const MAX_NOTE_LENGTH = 4000;

interface OptionGroup {
  readonly label: string;
  readonly options: readonly ModelOption[];
}

/** Text of a message without the reasoning block of thinking models. */
function withoutReasoning(text: string): string {
  return text.replace(/<think>[\s\S]*?(<\/think>|$)/g, '').trim();
}

@Component({
  selector: 'jv-chat',
  imports: [ConversationList, Icon, MarkdownPipe, RouterLink],
  templateUrl: './chat.html',
  styleUrl: './chat.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Chat {
  protected readonly store = inject(ChatStore);
  protected readonly memory = inject(MemoryStore);
  private readonly router = inject(Router);
  private readonly scroller = viewChild<ElementRef<HTMLElement>>('scroller');
  private stickToBottom = true;
  private noteFor: string | null | undefined = undefined;

  protected readonly automatic = AUTOMATIC;
  protected readonly maxLength = MAX_MESSAGE_LENGTH;
  protected readonly maxNoteLength = MAX_NOTE_LENGTH;
  protected readonly maxFactLength = MAX_FACT_LENGTH;
  protected readonly formatTokens = formatTokens;
  protected readonly draft = signal('');
  protected readonly noteOpen = signal(false);
  protected readonly noteDraft = signal('');
  protected readonly noteSaved = signal(false);
  protected readonly summaryOpen = signal(false);
  protected readonly rememberId = signal<string | null>(null);
  protected readonly rememberDraft = signal('');
  protected readonly rememberedId = signal<string | null>(null);
  protected readonly keyOf = (option: ModelOption): string =>
    optionKey(option.provider.id, option.model.id);

  protected readonly title = computed(() => {
    const conversation = this.store.conversation();
    if (conversation) {
      return conversation.title;
    }
    return this.store.temporary() ? 'New temporary chat' : 'New chat';
  });
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
    return (
      text.length > 0 &&
      text.length <= MAX_MESSAGE_LENGTH &&
      !this.store.busy() &&
      !this.store.readOnly()
    );
  });
  protected readonly nearLimit = computed(() => this.draft().length > MAX_MESSAGE_LENGTH * 0.8);
  /** Saved conversations can feed facts; temporary ones do not link back. */
  protected readonly canRemember = computed(() => this.store.savingEnabled());

  constructor() {
    void this.store.loadModels();
    void this.store.loadConversations();
    void this.memory.loadProjects();

    inject(ActivatedRoute)
      .queryParamMap.pipe(takeUntilDestroyed())
      .subscribe((params) => {
        const id = params.get('c');
        if (id && id !== this.store.conversation()?.id) {
          this.stickToBottom = true;
          void this.store.open(id);
        }
      });

    // Load the note when another conversation opens; keep unsaved edits
    // while the open one is updated (e.g. its title after a reply).
    effect(() => {
      const conversation = this.store.conversation();
      const id = conversation?.id ?? null;
      if (id !== this.noteFor) {
        this.noteFor = id;
        this.noteDraft.set(conversation?.pinnedNote ?? '');
        this.noteSaved.set(false);
      }
    });

    afterRenderEffect(() => {
      this.store.entries();
      const element = this.scroller()?.nativeElement;
      if (element && this.stickToBottom) {
        element.scrollTop = element.scrollHeight;
      }
    });
  }

  // ---- Conversations ----

  protected newChat(temporary: boolean): void {
    this.store.startNew(temporary, null);
    this.noteOpen.set(false);
    void this.router.navigate([], { queryParams: { c: null } });
  }

  protected onProject(event: Event): void {
    const value = (event.target as HTMLSelectElement).value;
    void this.store.setProject(value || null);
  }

  // ---- Pinned note (working memory) ----

  protected onNoteInput(event: Event): void {
    this.noteDraft.set((event.target as HTMLTextAreaElement).value);
    this.noteSaved.set(false);
  }

  protected async saveNote(): Promise<void> {
    if (await this.store.setPinnedNote(this.noteDraft().trim())) {
      this.noteSaved.set(true);
      const id = this.store.conversation()?.id;
      if (id) {
        void this.router.navigate([], { queryParams: { c: id } });
      }
    }
  }

  // ---- Remember (semantic memory) ----

  protected startRemember(entry: ChatEntry): void {
    this.rememberDraft.set(withoutReasoning(entry.content).slice(0, MAX_FACT_LENGTH));
    this.rememberId.set(entry.id);
  }

  protected onRememberInput(event: Event): void {
    this.rememberDraft.set((event.target as HTMLTextAreaElement).value);
  }

  protected async saveRemember(entry: ChatEntry): Promise<void> {
    const content = this.rememberDraft().trim();
    if (!content) {
      return;
    }
    const conversation = this.store.conversation();
    const fact = await this.memory.saveFact({
      id: null,
      content,
      projectId: this.store.projectId(),
      pinned: false,
      sourceConversationId: conversation && !conversation.temporary ? conversation.id : null,
    });
    if (fact) {
      this.rememberId.set(null);
      this.rememberedId.set(entry.id);
    }
  }

  // ---- Composer ----

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
    const isNew = !this.store.conversation();
    void this.store.send(text).then(() => {
      const id = this.store.conversation()?.id;
      if (isNew && id) {
        void this.router.navigate([], { queryParams: { c: id }, replaceUrl: true });
      }
    });
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

  /** Tooltip describing how the context of a reply was built. */
  protected contextDetails(context: ContextReport): string {
    const lines = [
      `Context window: ${context.windowTokens} tokens (${context.budgetTokens} for the prompt)`,
      `Prompt: ~${context.usedTokens} tokens`,
      `Earlier messages sent: ${context.historyIncluded}`,
    ];
    if (context.summarizedMessages) {
      lines.push(`Summarized messages: ${context.summarizedMessages}`);
    }
    if (context.historyOmitted) {
      lines.push(`Left out (no room): ${context.historyOmitted}`);
    }
    if (context.factsIncluded) {
      lines.push(`Facts: ${context.factsIncluded}`);
    }
    if (context.recallsIncluded) {
      lines.push(`From other conversations: ${context.recallsIncluded}`);
    }
    if (context.project) {
      lines.push(`Project: ${context.project}`);
    }
    if (context.noteTokens) {
      lines.push('Pinned note included');
    }
    if (context.memorySkipped) {
      lines.push(context.memorySkipped);
    }
    return lines.join('\n');
  }
}

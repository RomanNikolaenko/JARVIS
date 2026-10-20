import { ChangeDetectionStrategy, Component, inject, signal } from '@angular/core';
import { Router } from '@angular/router';
import type { Conversation } from '@jarvis/shared-types';

import { ChatStore } from '../../../core/state/chat.store';
import { formatShortDate } from '../../../shared/format';
import { Icon } from '../../../shared/icon/icon';

const SEARCH_DEBOUNCE_MS = 250;

/** Saved and temporary conversations: search, open, rename, delete. */
@Component({
  selector: 'jv-conversation-list',
  imports: [Icon],
  templateUrl: './conversation-list.html',
  styleUrl: './conversation-list.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class ConversationList {
  protected readonly store = inject(ChatStore);
  private readonly router = inject(Router);
  private searchTimer: ReturnType<typeof setTimeout> | undefined;

  protected readonly formatDate = formatShortDate;
  protected readonly renamingId = signal<string | null>(null);
  protected readonly renameDraft = signal('');
  protected readonly confirmDeleteId = signal<string | null>(null);

  protected newChat(temporary: boolean): void {
    this.store.startNew(temporary, null);
    void this.router.navigate([], { queryParams: { c: null } });
  }

  protected open(conversation: Conversation): void {
    void this.router.navigate([], { queryParams: { c: conversation.id } });
  }

  protected onSearch(event: Event): void {
    this.store.conversationQuery.set((event.target as HTMLInputElement).value);
    clearTimeout(this.searchTimer);
    this.searchTimer = setTimeout(() => void this.store.loadConversations(), SEARCH_DEBOUNCE_MS);
  }

  protected startRename(conversation: Conversation): void {
    this.confirmDeleteId.set(null);
    this.renameDraft.set(conversation.title);
    this.renamingId.set(conversation.id);
  }

  protected onRenameInput(event: Event): void {
    this.renameDraft.set((event.target as HTMLInputElement).value);
  }

  protected async saveRename(conversation: Conversation): Promise<void> {
    if (this.renamingId() !== conversation.id) {
      return;
    }
    const title = this.renameDraft().trim();
    this.renamingId.set(null);
    if (title && title !== conversation.title) {
      await this.store.rename(conversation.id, title);
    }
  }

  protected async remove(conversation: Conversation): Promise<void> {
    this.confirmDeleteId.set(null);
    const active = this.store.conversation()?.id === conversation.id;
    await this.store.remove(conversation.id);
    if (active) {
      void this.router.navigate([], { queryParams: { c: null } });
    }
  }
}

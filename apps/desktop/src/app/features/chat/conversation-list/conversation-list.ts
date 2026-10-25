import { ChangeDetectionStrategy, Component, DestroyRef, inject, signal } from '@angular/core';
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
  private readonly destroyRef = inject(DestroyRef);
  private searchTimer: ReturnType<typeof setTimeout> | undefined;

  constructor() {
    inject(DestroyRef).onDestroy(() => clearTimeout(this.searchTimer));
  }

  protected readonly formatDate = formatShortDate;
  protected readonly renamingId = signal<string | null>(null);
  protected readonly renameDraft = signal('');
  protected readonly renameSavingId = signal<string | null>(null);
  protected readonly renameError = signal<string | null>(null);
  private renameSession = 0;
  protected readonly confirmDeleteId = signal<string | null>(null);

  protected newChat(temporary: boolean): void {
    this.store.startNew(temporary, null);
    void this.router.navigate([], { queryParams: { c: null } });
  }

  protected open(conversation: Conversation): void {
    void this.router.navigate([], { queryParams: { c: conversation.id } });
  }

  protected onSearch(event: Event): void {
    this.store.setConversationQuery((event.target as HTMLInputElement).value);
    clearTimeout(this.searchTimer);
    this.searchTimer = setTimeout(() => void this.store.loadConversations(), SEARCH_DEBOUNCE_MS);
  }

  protected startRename(conversation: Conversation): void {
    ++this.renameSession;
    this.renameError.set(null);
    this.confirmDeleteId.set(null);
    this.renameDraft.set(conversation.title);
    this.renamingId.set(conversation.id);
  }

  protected onRenameInput(event: Event): void {
    this.renameDraft.set((event.target as HTMLInputElement).value);
    this.renameError.set(null);
  }

  protected cancelRename(): void {
    ++this.renameSession;
    this.renamingId.set(null);
    this.renameError.set(null);
  }

  protected async saveRename(conversation: Conversation): Promise<void> {
    if (this.renamingId() !== conversation.id || this.renameSavingId() !== null) {
      return;
    }
    const draft = this.renameDraft();
    const title = draft.trim();
    if (!title) {
      this.renameError.set('Enter a conversation title.');
      return;
    }
    if (title === conversation.title) {
      this.cancelRename();
      return;
    }
    const session = this.renameSession;
    this.renameSavingId.set(conversation.id);
    this.renameError.set(null);
    try {
      const saved = await this.store.rename(conversation.id, title);
      if (this.destroyRef.destroyed || session !== this.renameSession || this.renamingId() !== conversation.id) return;
      if (!saved) this.renameError.set('Could not save the title. Press Enter to retry.');
      else if (this.renameDraft() === draft) this.cancelRename();
    } finally {
      this.renameSavingId.set(null);
    }
  }

  protected async remove(conversation: Conversation): Promise<void> {
    this.confirmDeleteId.set(null);
    const result = await this.store.remove(conversation.id);
    if (result === 'closed' && !this.destroyRef.destroyed && this.store.conversation() === null) {
      void this.router.navigate([], { queryParams: { c: null } });
    }
  }
}

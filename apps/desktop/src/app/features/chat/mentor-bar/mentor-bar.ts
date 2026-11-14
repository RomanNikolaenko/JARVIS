import { ChangeDetectionStrategy, Component, effect, inject, signal, untracked } from '@angular/core';

import { IpcError } from '../../../core/ipc/ipc-error';
import { JarvisApi } from '../../../core/ipc/jarvis-api';
import { MentorStore } from '../../../core/state/mentor.store';

/**
 * Mentor mode in the chat: the lesson's state with its controls, or the
 * button and form that start a lesson in a project folder.
 */
@Component({
  selector: 'jv-mentor-bar',
  templateUrl: './mentor-bar.html',
  styleUrl: './mentor-bar.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class MentorBar {
  protected readonly mentor = inject(MentorStore);
  private readonly api = inject(JarvisApi);

  protected readonly folders = signal<readonly string[]>([]);
  protected readonly folder = signal('');
  protected readonly error = signal<string | null>(null);
  protected readonly busy = signal(false);

  constructor() {
    effect(() => {
      if (this.mentor.starting()) untracked(() => void this.loadFolders());
    });
  }

  protected async addFolder(): Promise<void> {
    try {
      const settings = await this.api.addWorkspaceFolder();
      if (settings) this.setFolders(settings.tools.workspaceFolders, true);
    } catch (error) {
      this.error.set(IpcError.from(error).message);
    }
  }

  protected onFolder(event: Event): void {
    this.folder.set((event.target as HTMLSelectElement).value);
  }

  protected async begin(): Promise<void> {
    if (!this.folder() || this.busy()) return;
    this.busy.set(true);
    try {
      await this.mentor.begin(this.folder());
    } finally {
      this.busy.set(false);
    }
  }

  private async loadFolders(): Promise<void> {
    try {
      this.setFolders((await this.api.getSettings()).tools.workspaceFolders, false);
      this.error.set(null);
    } catch (error) {
      this.error.set(IpcError.from(error).message);
    }
  }

  private setFolders(folders: readonly string[], pickLast: boolean): void {
    this.folders.set(folders);
    if (pickLast && folders.length) this.folder.set(folders[folders.length - 1]);
    else if (!folders.includes(this.folder())) this.folder.set(folders[0] ?? '');
  }
}

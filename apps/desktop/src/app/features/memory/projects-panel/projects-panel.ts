import { ChangeDetectionStrategy, Component, DestroyRef, computed, inject, signal } from '@angular/core';
import { Router } from '@angular/router';
import type { Project } from '@jarvis/shared-types';

import { ChatStore } from '../../../core/state/chat.store';
import { MemoryStore } from '../../../core/state/memory.store';
import { HudPanel } from '../../../shared/hud-panel/hud-panel';
import { Icon } from '../../../shared/icon/icon';

const MAX_NAME = 100;
const MAX_DESCRIPTION = 2000;
const MAX_INSTRUCTIONS = 8000;

/** Projects (project memory): instructions and facts shared by their chats. */
@Component({
  selector: 'jv-projects-panel',
  imports: [HudPanel, Icon],
  templateUrl: './projects-panel.html',
  styleUrl: '../memory-shared.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class ProjectsPanel {
  protected readonly store = inject(MemoryStore);
  private readonly chat = inject(ChatStore);
  private readonly router = inject(Router);
  private readonly destroyRef = inject(DestroyRef);
  private revision = 0;

  protected readonly limits = {
    name: MAX_NAME,
    description: MAX_DESCRIPTION,
    instructions: MAX_INSTRUCTIONS,
  };
  /** `null`: no form; `''`: a new project; otherwise the edited id. */
  protected readonly editingId = signal<string | null>(null);
  protected readonly name = signal('');
  protected readonly description = signal('');
  protected readonly instructions = signal('');
  protected readonly confirmDeleteId = signal<string | null>(null);
  protected readonly editorSession = signal(0);
  protected readonly saving = signal(false);
  protected readonly canSave = computed(() => {
    const length = this.name().trim().length;
    return (
      this.editingId() !== null &&
      length > 0 &&
      length <= MAX_NAME &&
      this.description().trim().length <= MAX_DESCRIPTION &&
      this.instructions().trim().length <= MAX_INSTRUCTIONS &&
      !this.store.busy() &&
      !this.saving()
    );
  });

  protected create(): void {
    this.fill('', '', '', '');
  }

  protected edit(project: Project): void {
    this.fill(project.id, project.name, project.description, project.instructions);
  }

  protected cancel(): void {
    this.editorSession.update((session) => session + 1);
    this.editingId.set(null);
  }

  protected onField(field: 'name' | 'description' | 'instructions', event: Event): void {
    ++this.revision;
    this[field].set(this.valueOf(event));
  }

  protected async save(renderedSession: number): Promise<void> {
    if (this.destroyRef.destroyed || renderedSession !== this.editorSession() || !this.canSave()) {
      return;
    }
    const session = this.editorSession();
    const revision = this.revision;
    const id = this.editingId();
    this.saving.set(true);
    try {
      const saved = await this.store.saveProject({
        id: id || null,
        name: this.name().trim(),
        description: this.description().trim(),
        instructions: this.instructions().trim(),
      });
      if (!saved || this.destroyRef.destroyed || session !== this.editorSession()) return;
      // A first save already created this project. Newer edits update its ID
      // on the next explicit save instead of creating a second project.
      if (id === '') this.editingId.set(saved.id);
      if (revision === this.revision) this.cancel();
    } finally {
      this.saving.set(false);
    }
  }

  protected async remove(project: Project): Promise<void> {
    if (this.destroyRef.destroyed || this.store.busy() || this.saving() || !this.store.projects().includes(project)) return;
    this.confirmDeleteId.set(null);
    await this.store.deleteProject(project.id);
  }

  protected startChat(project: Project): void {
    this.chat.startNew(false, project.id);
    void this.router.navigate(['/chat']);
  }

  protected valueOf(event: Event): string {
    return (event.target as HTMLInputElement).value;
  }

  private fill(id: string, name: string, description: string, instructions: string): void {
    if (this.destroyRef.destroyed) return;
    this.editorSession.update((session) => session + 1);
    this.name.set(name);
    this.description.set(description);
    this.instructions.set(instructions);
    this.editingId.set(id);
  }
}

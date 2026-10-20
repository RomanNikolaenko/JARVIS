import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
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
  protected readonly canSave = computed(() => this.name().trim().length > 0 && !this.store.busy());

  protected create(): void {
    this.fill('', '', '', '');
  }

  protected edit(project: Project): void {
    this.fill(project.id, project.name, project.description, project.instructions);
  }

  protected cancel(): void {
    this.editingId.set(null);
  }

  protected async save(): Promise<void> {
    if (!this.canSave()) {
      return;
    }
    const saved = await this.store.saveProject({
      id: this.editingId() || null,
      name: this.name().trim(),
      description: this.description().trim(),
      instructions: this.instructions().trim(),
    });
    if (saved) {
      this.editingId.set(null);
    }
  }

  protected async remove(project: Project): Promise<void> {
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
    this.name.set(name);
    this.description.set(description);
    this.instructions.set(instructions);
    this.editingId.set(id);
  }
}

import { ChangeDetectionStrategy, Component, inject, signal } from '@angular/core';

import { MemoryStore } from '../../../core/state/memory.store';
import { HudPanel } from '../../../shared/hud-panel/hud-panel';
import { Icon } from '../../../shared/icon/icon';

/** The action history (episodic memory). */
@Component({
  selector: 'jv-history-panel',
  imports: [HudPanel, Icon],
  templateUrl: './history-panel.html',
  styleUrls: ['../memory-shared.scss', './history-panel.scss'],
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class HistoryPanel {
  protected readonly store = inject(MemoryStore);
  protected readonly confirmClear = signal(false);

  protected time(ms: number): string {
    return new Date(ms).toLocaleString(undefined, {
      day: 'numeric',
      month: 'short',
      hour: '2-digit',
      minute: '2-digit',
      hour12: false,
    });
  }

  protected async clear(): Promise<void> {
    this.confirmClear.set(false);
    await this.store.clearEpisodes();
  }
}

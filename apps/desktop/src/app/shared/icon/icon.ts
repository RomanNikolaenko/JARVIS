import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';

/** Stroke icons on a 24×24 grid, drawn for JARVIS. */
const ICONS = {
  dashboard: ['M3 3h8v8H3z', 'M13 3h8v5h-8z', 'M13 10h8v11h-8z', 'M3 13h8v8H3z'],
  chat: ['M4 5h16v11H9l-5 4z', 'M8 9h8', 'M8 12h5'],
  events: ['M3 12h4l3-7 4 14 3-7h4'],
  settings: ['M4 6h9M17 6h3M15 4v4', 'M4 12h3M11 12h9M9 10v4', 'M4 18h11M19 18h1M17 16v4'],
  mic: ['M12 3a3 3 0 0 1 3 3v6a3 3 0 0 1-6 0V6a3 3 0 0 1 3-3z', 'M5 11a7 7 0 0 0 14 0', 'M12 18v3'],
  image: ['M3 5h18v14H3z', 'M3 15l5-5 4 4 3-3 6 6', 'M15 9h.01'],
  camera: ['M4 7h4l2-2h4l2 2h4v12H4z', 'M12 10a3 3 0 1 0 0 6 3 3 0 0 0 0-6z'],
  volume: ['M4 9h4l5-4v14l-5-4H4z', 'M16 9a4 4 0 0 1 0 6', 'M19 6a8 8 0 0 1 0 12'],
  micOff: ['M9 6a3 3 0 0 1 6 0v6a3 3 0 0 1-.6 1.8', 'M5 11a7 7 0 0 0 11.5 5.4', 'M12 18v3', 'M3 3l18 18'],
  cpu: ['M7 7h10v10H7z', 'M10 10h4v4h-4z', 'M9 2v3M15 2v3M9 19v3M15 19v3M2 9h3M2 15h3M19 9h3M19 15h3'],
  memory: ['M3 8h18v8H3z', 'M7 8v8M11 8v8M15 8v8', 'M5 16v3M19 16v3'],
  send: ['M21 3L10 14', 'M21 3l-7 18-4-7-7-4z'],
  stop: ['M7 7h10v10H7z'],
  trash: ['M4 7h16', 'M9 7V4h6v3', 'M6 7l1 13h10l1-13'],
  warning: ['M12 3l10 18H2z', 'M12 10v5', 'M12 18v.01'],
  check: ['M5 12l5 5L20 7'],
  refresh: ['M20 11a8 8 0 1 0-2.3 5.7', 'M20 4v7h-7'],
  shield: ['M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6z'],
  spark: ['M12 3v4M12 17v4M3 12h4M17 12h4', 'M6 6l2.5 2.5M15.5 15.5L18 18M6 18l2.5-2.5M15.5 8.5L18 6'],
  monitor: ['M3 4h18v12H3z', 'M8 20h8M12 16v4'],
  brain: [
    'M9 4a3 3 0 0 0-3 3 3 3 0 0 0-2 5 3 3 0 0 0 2 5 3 3 0 0 0 3 3h1V4z',
    'M15 4a3 3 0 0 1 3 3 3 3 0 0 1 2 5 3 3 0 0 1-2 5 3 3 0 0 1-3 3h-1V4z',
    'M10 9H8M14 9h2M10 15H8M14 15h2',
  ],
  plus: ['M12 5v14M5 12h14'],
  pin: ['M9 3h6l-1 6 3 3H7l3-3z', 'M12 12v9'],
  edit: ['M4 20h4L19 9l-4-4L4 16z', 'M13 7l4 4'],
  search: ['M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14z', 'M16 16l5 5'],
  download: ['M12 4v11', 'M7 10l5 5 5-5', 'M4 20h16'],
  upload: ['M12 20V9', 'M7 14l5-5 5 5', 'M4 4h16'],
  clock: ['M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z', 'M12 7v5l3 2'],
  folder: ['M3 6h6l2 2h10v11H3z'],
  agent: ['M6 8h12v11H6z', 'M12 4v4', 'M10 13h.01M14 13h.01', 'M10 16h4', 'M3 12v3M21 12v3'],
  ghost: ['M5 20V10a7 7 0 0 1 14 0v10l-2.5-2-2.5 2-2-2-2 2-2.5-2z', 'M9.5 10h.01M14.5 10h.01'],
} as const satisfies Record<string, readonly string[]>;

export type IconName = keyof typeof ICONS;

@Component({
  selector: 'jv-icon',
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { class: 'jv-icon', 'aria-hidden': 'true' },
  template: `
    <svg
      viewBox="0 0 24 24"
      [attr.width]="size()"
      [attr.height]="size()"
      fill="none"
      stroke="currentColor"
      stroke-width="1.6"
      stroke-linecap="round"
      stroke-linejoin="round">
      @for (d of paths(); track $index) {
        <path [attr.d]="d" />
      }
    </svg>
  `,
  styles: `
    :host {
      display: inline-flex;
      flex-shrink: 0;
    }
  `,
})
export class Icon {
  readonly name = input.required<IconName>();
  readonly size = input(18);
  protected readonly paths = computed(() => ICONS[this.name()]);
}

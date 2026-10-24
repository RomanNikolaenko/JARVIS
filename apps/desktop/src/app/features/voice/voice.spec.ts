import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import type { Settings } from '@jarvis/shared-types';

import { JarvisApi } from '../../core/ipc/jarvis-api';
import { SettingsStore } from '../../core/state/settings.store';
import { SETTINGS } from '../../testing/settings.fixture';
import { voicePackage, voiceStatus } from '../../testing/voice.fixture';
import { VoicePage, levelPercent } from './voice';

describe('VoicePage', () => {
  let saved: Settings[];
  const api = {
    connected: true,
    onVoiceState: vi.fn(async () => () => undefined),
    onVoiceTranscript: vi.fn(async () => () => undefined),
    onVoiceLevel: vi.fn(async () => () => undefined),
    getVoiceStatus: vi.fn(async () => voiceStatus()),
    installVoicePackage: vi.fn(async () => 'download-1'),
    removeVoicePackage: vi.fn(async () => undefined),
    speakText: vi.fn(async () => undefined),
    updateSettings: vi.fn(async (settings: Settings) => {
      saved.push(settings);
      return settings;
    }),
  };

  async function render() {
    saved = [];
    TestBed.configureTestingModule({
      imports: [VoicePage],
      providers: [provideRouter([]), { provide: JarvisApi, useValue: api }],
    });
    TestBed.inject(SettingsStore).settings.set(SETTINGS);
    const fixture = TestBed.createComponent(VoicePage);
    await fixture.whenStable();
    fixture.detectChanges();
    return { fixture, element: fixture.nativeElement as HTMLElement };
  }

  function button(element: HTMLElement, text: string): HTMLButtonElement {
    const found = [...element.querySelectorAll('button')].find((b) => b.textContent?.trim().startsWith(text));
    if (!found) {
      throw new Error(`no button "${text}"`);
    }
    return found;
  }

  beforeEach(() => vi.clearAllMocks());

  it('asks before downloading a package and shows where it comes from', async () => {
    const { fixture, element } = await render();
    const row = [...element.querySelectorAll('tr')].find((r) => r.textContent?.includes('piper description'));
    expect(row).toBeDefined();
    button(row!, 'Install').click();
    fixture.detectChanges();
    expect(api.installVoicePackage).not.toHaveBeenCalled();
    expect(row!.textContent).toContain('github.com/example, release 1');
    expect(row!.textContent).toContain('programs that JARVIS will run');
    button(row!, 'Download').click();
    await fixture.whenStable();
    expect(api.installVoicePackage).toHaveBeenCalledWith('piper');
  });

  it('removes a package only after confirmation', async () => {
    const { fixture, element } = await render();
    const row = [...element.querySelectorAll('tr')].find((r) => r.textContent?.includes('whisper-small-q5_1 description'));
    button(row!, 'Remove').click();
    fixture.detectChanges();
    expect(api.removeVoicePackage).not.toHaveBeenCalled();
    button(row!, 'Delete').click();
    await fixture.whenStable();
    expect(api.removeVoicePackage).toHaveBeenCalledWith('whisper-small-q5_1');
  });

  it('saves voice settings, including the wake word', async () => {
    const { fixture, element } = await render();
    const wakeWord = [...element.querySelectorAll<HTMLInputElement>('input[type=checkbox]')].find((input) =>
      input.parentElement?.textContent?.includes('wake word')
    );
    wakeWord!.click();
    fixture.detectChanges();
    button(element, 'Save').click();
    await fixture.whenStable();
    expect(saved).toHaveLength(1);
    expect(saved[0].voice).toEqual({ ...SETTINGS.voice, wakeWord: true });
  });

  it('offers the speakers of an installed multi-speaker voice', async () => {
    api.getVoiceStatus.mockResolvedValue(
      voiceStatus({
        packages: [
          voicePackage('piper-uk', 'voice', true, { language: 'uk', speakers: ['Лада', 'Микита', 'Тетяна'], recommended: true }),
        ],
        canSpeak: true,
      })
    );
    const { fixture, element } = await render();
    await fixture.whenStable();
    fixture.detectChanges();
    const options = [...element.querySelectorAll('option')].map((o) => o.textContent?.trim());
    expect(options).toEqual(expect.arrayContaining(['Лада', 'Микита', 'Тетяна']));
    button(element, 'Try Ukrainian').click();
    expect(api.speakText).toHaveBeenCalledWith(expect.stringContaining('Джарвіс'));
  });

  it('scales the microphone level for the meter', () => {
    expect(levelPercent(0)).toBe(0);
    expect(levelPercent(0.04)).toBe(44);
    expect(levelPercent(1)).toBe(100);
    expect(levelPercent(-1)).toBe(0);
  });
});

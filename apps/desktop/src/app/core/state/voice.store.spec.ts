import { TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
import type { CaptureInfo, VoiceState, VoiceTranscript } from '@jarvis/shared-types';

import { IDLE_VOICE, voiceStatus } from '../../testing/voice.fixture';
import { JarvisApi } from '../ipc/jarvis-api';
import { ChatStore } from './chat.store';
import { VisionStore } from './vision.store';
import { VoiceStore } from './voice.store';

const CAPTURE: CaptureInfo = {
  id: 'capture-1',
  source: 'screen',
  label: 'Display 1',
  width: 1568,
  height: 882,
  preview: 'data:image/jpeg;base64,AAAA',
  capturedAtMs: 1,
};

describe('VoiceStore', () => {
  let onState: (state: VoiceState) => void;
  let onTranscript: (transcript: VoiceTranscript) => void;
  let onLevel: (level: number) => void;
  let api: Record<string, ReturnType<typeof vi.fn>>;
  let chat: { busy: ReturnType<typeof vi.fn>; cancel: ReturnType<typeof vi.fn>; send: ReturnType<typeof vi.fn> };
  let store: VoiceStore;

  beforeEach(async () => {
    api = {
      onVoiceState: vi.fn(async (handler: (state: VoiceState) => void) => {
        onState = handler;
        return () => undefined;
      }),
      onVoiceTranscript: vi.fn(async (handler: (transcript: VoiceTranscript) => void) => {
        onTranscript = handler;
        return () => undefined;
      }),
      onVoiceLevel: vi.fn(async (handler: (level: number) => void) => {
        onLevel = handler;
        return () => undefined;
      }),
      getVoiceStatus: vi.fn(async () => voiceStatus()),
      startListening: vi.fn(async () => undefined),
      stopListening: vi.fn(async () => undefined),
      installVoicePackage: vi.fn(async () => 'download-1'),
    };
    chat = {
      busy: vi.fn(() => false),
      cancel: vi.fn(async () => undefined),
      send: vi.fn(async () => undefined),
    };
    TestBed.configureTestingModule({
      providers: [
        provideRouter([]),
        { provide: JarvisApi, useValue: api },
        { provide: ChatStore, useValue: chat },
        { provide: VisionStore, useValue: { take: () => [CAPTURE] } },
      ],
    });
    store = TestBed.inject(VoiceStore);
    await store.start();
  });

  const transcript = (text: string, source: VoiceTranscript['source']): VoiceTranscript => ({
    text,
    source,
    language: 'uk',
  });

  it('loads the status and follows the engine state and level', () => {
    expect(store.available()).toBe(true);
    expect(store.canListen()).toBe(true);
    expect(store.canSpeak()).toBe(false);
    onState({ ...IDLE_VOICE, phase: 'listening', micOpen: true });
    onLevel(0.2);
    expect(store.micOpen()).toBe(true);
    expect(store.level()).toBe(0.2);
    // The level drops to zero when the microphone closes.
    onState(IDLE_VOICE);
    expect(store.level()).toBe(0);
  });

  it('sends spoken requests to the chat and asks for a spoken reply', async () => {
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    onTranscript(transcript('Яка погода?', 'wakeWord'));
    await vi.waitFor(() => expect(chat.send).toHaveBeenCalledWith('Яка погода?', true, [CAPTURE]));
    expect(navigate).toHaveBeenCalledWith('/chat');
  });

  it('cancels a reply that is still streaming before sending a new request', async () => {
    vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    let busy = true;
    chat.busy.mockImplementation(() => busy);
    chat.cancel.mockImplementation(async () => {
      busy = false;
    });
    onTranscript(transcript('Стоп, інше питання', 'pushToTalk'));
    await vi.waitFor(() => expect(chat.send).toHaveBeenCalled());
    expect(chat.cancel).toHaveBeenCalledBefore(chat.send);
  });

  it('keeps a microphone test out of the chat', async () => {
    await store.listen(true);
    expect(api['startListening']).toHaveBeenCalled();
    onTranscript(transcript('раз, два, три', 'pushToTalk'));
    expect(store.testTranscript()).toBe('раз, два, три');
    expect(chat.send).not.toHaveBeenCalled();
  });

  it('shows voice problems briefly', async () => {
    vi.useFakeTimers();
    try {
      onState({ ...IDLE_VOICE, lastError: 'no speech was heard' });
      expect(store.notice()).toBe('no speech was heard');
      api['startListening'].mockRejectedValueOnce({ code: 'notFound', message: 'no Whisper model is installed' });
      await store.listen();
      expect(store.notice()).toBe('no Whisper model is installed');
      vi.advanceTimersByTime(7_000);
      expect(store.notice()).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });
});

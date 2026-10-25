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
  let chat: {
    busy: ReturnType<typeof vi.fn<() => boolean>>;
    cancel: ReturnType<typeof vi.fn>;
    send: ReturnType<typeof vi.fn>;
    inputContext: ReturnType<typeof vi.fn>;
    acceptsInput: ReturnType<typeof vi.fn>;
  };
  let take: ReturnType<typeof vi.fn>;
  let store: VoiceStore;
  let stops: ReturnType<typeof vi.fn>[];

  beforeEach(async () => {
    stops = [];
    api = {
      onVoiceState: vi.fn(async (handler: (state: VoiceState) => void) => {
        onState = handler;
        const stop = vi.fn();
        stops.push(stop);
        return stop;
      }),
      onVoiceTranscript: vi.fn(async (handler: (transcript: VoiceTranscript) => void) => {
        onTranscript = handler;
        const stop = vi.fn();
        stops.push(stop);
        return stop;
      }),
      onVoiceLevel: vi.fn(async (handler: (level: number) => void) => {
        onLevel = handler;
        const stop = vi.fn();
        stops.push(stop);
        return stop;
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
      inputContext: vi.fn(() => 0),
      acceptsInput: vi.fn(() => !chat.busy()),
    };
    take = vi.fn(() => [CAPTURE]);
    TestBed.configureTestingModule({
      providers: [
        provideRouter([]),
        { provide: JarvisApi, useValue: api },
        { provide: ChatStore, useValue: chat },
        { provide: VisionStore, useValue: { take } },
      ],
    });
    store = TestBed.inject(VoiceStore);
    await store.start();
  });

  const transcript = (text: string, source: VoiceTranscript['source'], captureId = 'phrase-1'): VoiceTranscript => {
    onState({ ...IDLE_VOICE, captureId, micOpen: true, hearingSpeech: true });
    return { text, source, language: 'uk', captureId };
  };

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

  it('does not send push-to-talk into a different conversation', async () => {
    await store.listen();
    chat.inputContext.mockReturnValue(1);
    onTranscript(transcript('old request', 'pushToTalk'));
    await Promise.resolve();
    expect(chat.send).not.toHaveBeenCalled();
    expect(take).not.toHaveBeenCalled();
  });

  it('ignores a transcript after cancelling listening', async () => {
    await store.listen();
    await store.cancelListening();
    onTranscript(transcript('cancelled request', 'pushToTalk'));
    expect(chat.send).not.toHaveBeenCalled();
  });

  it('retains attachments when a previous reply cannot be stopped', async () => {
    vi.useFakeTimers();
    try {
      chat.busy.mockReturnValue(true);
      onTranscript(transcript('next request', 'wakeWord'));
      await vi.advanceTimersByTimeAsync(5100);
      expect(chat.send).not.toHaveBeenCalled();
      expect(take).not.toHaveBeenCalled();
      expect(store.notice()).toContain('not ready');
    } finally {
      vi.useRealTimers();
    }
  });

  it('does not send after navigation changes the conversation', async () => {
    vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockImplementation(async () => {
      chat.inputContext.mockReturnValue(1);
      return true;
    });
    onTranscript(transcript('request', 'wakeWord'));
    await Promise.resolve();
    expect(chat.send).not.toHaveBeenCalled();
    expect(take).not.toHaveBeenCalled();
  });

  it('rejects unknown captures and consumes each known capture once', async () => {
    vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    onTranscript({ text: 'unknown', source: 'wakeWord', language: 'uk', captureId: 'unknown' });
    expect(chat.send).not.toHaveBeenCalled();
    const result = transcript('request', 'wakeWord');
    onTranscript(result);
    await vi.waitFor(() => expect(chat.send).toHaveBeenCalledTimes(1));
    onTranscript(result);
    await Promise.resolve();
    expect(chat.send).toHaveBeenCalledTimes(1);
  });

  it('keeps wake-word phrases bound to the conversation observed at capture', async () => {
    const old = transcript('old request', 'wakeWord');
    chat.inputContext.mockReturnValue(1);
    onTranscript(old);
    expect(chat.send).not.toHaveBeenCalled();
    expect(take).not.toHaveBeenCalled();
  });

  it('does not associate an old result with a newer microphone test', async () => {
    await store.listen();
    const old = transcript('old request', 'pushToTalk');
    await store.cancelListening();
    await store.listen(true);
    const test = transcript('test phrase', 'pushToTalk', 'phrase-2');
    onTranscript(old);
    expect(store.testTranscript()).toBeNull();
    onTranscript(test);
    expect(store.testTranscript()).toBe('test phrase');
    expect(chat.send).not.toHaveBeenCalled();
  });
  it('handles a transcript arriving before its capture state', async () => {
    vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    await store.listen();
    onTranscript({ text: 'request', source: 'pushToTalk', language: 'uk', captureId: 'late' });
    onState({ ...IDLE_VOICE, captureId: 'late', micOpen: true });
    await vi.waitFor(() => expect(chat.send).toHaveBeenCalledTimes(1));
  });
  it('does not move a buffered transcript to another conversation', async () => {
    onTranscript({ text: 'request', source: 'wakeWord', language: 'uk', captureId: 'late' });
    chat.inputContext.mockReturnValue(1);
    onState({ ...IDLE_VOICE, captureId: 'late', micOpen: true });
    await Promise.resolve();
    expect(chat.send).not.toHaveBeenCalled();
  });
  it('clears microphone test intent when starting fails', async () => {
    vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    api['startListening'].mockRejectedValueOnce(new Error('device unavailable'));
    await store.listen(true);
    onTranscript(transcript('real request', 'wakeWord', 'after-failure'));
    await vi.waitFor(() => expect(chat.send).toHaveBeenCalledTimes(1));
    expect(store.testTranscript()).toBeNull();
  });
  it('preserves newer microphone events over stale status responses', async () => {
    let resolve!: (value: ReturnType<typeof voiceStatus>) => void;
    api['getVoiceStatus'].mockReturnValueOnce(
      new Promise((done) => {
        resolve = done;
      })
    );
    const refresh = store.refresh();
    onState({ ...IDLE_VOICE, micOpen: true });
    resolve(voiceStatus());
    await refresh;
    expect(store.micOpen()).toBe(true);
  });
  it('releases subscriptions on destruction', () => {
    TestBed.resetTestingModule();
    onState({ ...IDLE_VOICE, micOpen: true });
    expect(store.micOpen()).toBe(false);
    expect(stops).toHaveLength(3);
    for (const stop of stops) expect(stop).toHaveBeenCalledTimes(1);
  });
  it('releases partial voice subscriptions before retrying', async () => {
    TestBed.resetTestingModule();
    stops = [];
    TestBed.configureTestingModule({
      providers: [
        provideRouter([]),
        { provide: JarvisApi, useValue: api },
        { provide: ChatStore, useValue: chat },
        { provide: VisionStore, useValue: { take } },
      ],
    });
    store = TestBed.inject(VoiceStore);
    api['onVoiceTranscript'].mockRejectedValueOnce(new Error('registration failed'));
    await store.start();
    expect(stops).toHaveLength(1);
    expect(stops[0]).toHaveBeenCalledTimes(1);
    await store.start();
    expect(stops).toHaveLength(4);
    TestBed.resetTestingModule();
    for (const stop of stops) expect(stop).toHaveBeenCalledTimes(1);
  });
  it('releases a voice subscription registered after destruction', async () => {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      providers: [
        provideRouter([]),
        { provide: JarvisApi, useValue: api },
        { provide: ChatStore, useValue: chat },
        { provide: VisionStore, useValue: { take } },
      ],
    });
    store = TestBed.inject(VoiceStore);
    let resolve!: (value: () => void) => void;
    const stop = vi.fn();
    api['onVoiceState'].mockReturnValueOnce(
      new Promise((done) => {
        resolve = done;
      })
    );
    api['onVoiceTranscript'].mockClear();
    const start = store.start();
    TestBed.resetTestingModule();
    resolve(stop);
    await start;
    expect(stop).toHaveBeenCalledTimes(1);
    expect(api['onVoiceTranscript']).not.toHaveBeenCalled();
  });
});

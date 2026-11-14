import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import type { CaptureInfo, MentorNudge, MentorStatus, PendingApproval, VoiceState, VoiceTranscript } from '@jarvis/shared-types';

import { JarvisApi } from '../ipc/jarvis-api';
import { AgentsStore } from './agents.store';
import { ChatStore } from './chat.store';
import { MENTOR_OFF, MentorStore } from './mentor.store';
import { VisionStore } from './vision.store';

const LISTENING: MentorStatus = {
  ...MENTOR_OFF,
  conversationId: 'c-1',
  projectFolder: 'D:/work/shop',
  phase: 'listening',
};

const SHOT: CaptureInfo = {
  id: 'shot-1',
  source: 'screen',
  label: 'Display 1',
  width: 100,
  height: 100,
  preview: 'data:image/jpeg;base64,AAAA',
  capturedAtMs: 1,
};

function phrase(text: string): VoiceTranscript {
  return { captureId: crypto.randomUUID(), text, source: 'conversation', language: 'uk' };
}

function approval(risk: PendingApproval['risk']): PendingApproval {
  return {
    id: 'a-1',
    taskId: null,
    tool: 'write_file',
    risk,
    summary: 'Create src/cart.ts',
    reason: 'The mentor wants to type this change into your project',
    details: 'code',
    requestedAtMs: Date.now(),
    expiresAtMs: Date.now() + 60_000,
  };
}

const COMMANDS: Record<string, string> = {
  пауза: 'pause',
  'закінчуємо урок': 'end',
  'подивись на екран': 'look',
  стоп: 'stop',
  так: 'yes',
  ні: 'no',
  продовжуємо: 'resume',
};

describe('MentorStore', () => {
  let api: Record<string, ReturnType<typeof vi.fn>>;
  let onStatus: (status: MentorStatus) => void;
  let onNudge: (nudge: MentorNudge) => void;
  let chat: {
    busy: ReturnType<typeof signal<boolean>>;
    conversation: ReturnType<typeof signal<{ id: string } | null>>;
    selection: ReturnType<typeof signal<null>>;
    send: ReturnType<typeof vi.fn>;
  };
  let approvals: ReturnType<typeof signal<readonly PendingApproval[]>>;
  let resolve: ReturnType<typeof vi.fn>;
  let captureScreen: ReturnType<typeof vi.fn>;
  let consume: ReturnType<typeof vi.fn>;
  let store: MentorStore;

  beforeEach(async () => {
    api = {
      onMentorState: vi.fn(async (handler: (status: MentorStatus) => void) => {
        onStatus = handler;
        return vi.fn();
      }),
      onMentorNudge: vi.fn(async (handler: (nudge: MentorNudge) => void) => {
        onNudge = handler;
        return vi.fn();
      }),
      onVoiceState: vi.fn(async (_handler: (state: VoiceState) => void) => vi.fn()),
      mentorStatus: vi.fn(async () => LISTENING),
      mentorCommand: vi.fn(async (text: string) => COMMANDS[text.toLowerCase()] ?? null),
      mentorPause: vi.fn(async () => undefined),
      mentorResume: vi.fn(async () => LISTENING),
      mentorStop: vi.fn(async () => undefined),
      mentorStopTyping: vi.fn(async () => undefined),
      mentorReportBusy: vi.fn(async () => undefined),
      mentorOwnerSpoke: vi.fn(async () => undefined),
      stopSpeaking: vi.fn(async () => undefined),
    };
    chat = {
      busy: signal(false),
      conversation: signal({ id: 'c-1' }),
      selection: signal(null),
      send: vi.fn(async () => 'sent'),
    };
    approvals = signal<readonly PendingApproval[]>([]);
    resolve = vi.fn(async () => null);
    consume = vi.fn();
    captureScreen = vi.fn(async () => undefined);
    TestBed.configureTestingModule({
      providers: [
        { provide: JarvisApi, useValue: api },
        { provide: ChatStore, useValue: chat },
        { provide: AgentsStore, useValue: { approvals, resolve } },
        {
          provide: VisionStore,
          useValue: { captureScreen, prepareSend: () => ({ attachments: [SHOT], consume }) },
        },
      ],
    });
    store = TestBed.inject(MentorStore);
    await store.start();
  });

  it('holds phrases until the reply ends', async () => {
    chat.busy.set(true);
    TestBed.tick();
    await store.handlePhrase(phrase('Що таке компонент?'));
    await store.handlePhrase(phrase('І навіщо сигнали?'));
    expect(chat.send).not.toHaveBeenCalled();
    expect(store.held()).toEqual(['Що таке компонент?', 'І навіщо сигнали?']);

    chat.busy.set(false);
    TestBed.tick();
    await vi.waitFor(() => expect(chat.send).toHaveBeenCalledTimes(1));
    expect(chat.send).toHaveBeenCalledWith('Що таке компонент?\nІ навіщо сигнали?', true, [], undefined, 'phrase');
    expect(store.held()).toEqual([]);
    expect(api['mentorReportBusy']).toHaveBeenCalledWith(true);
  });

  it('commands never reach the model', async () => {
    await store.handlePhrase(phrase('Пауза'));
    expect(api['mentorPause']).toHaveBeenCalled();
    await store.handlePhrase(phrase('Закінчуємо урок'));
    expect(api['mentorStop']).toHaveBeenCalled();
    await store.handlePhrase(phrase('Стоп'));
    expect(api['stopSpeaking']).toHaveBeenCalled();
    expect(chat.send).not.toHaveBeenCalled();

    // Look: the next phrase carries a screenshot.
    await store.handlePhrase(phrase('Подивись на екран'));
    expect(captureScreen).toHaveBeenCalled();
    expect(chat.send).not.toHaveBeenCalled();
    await store.handlePhrase(phrase('Що тут не так?'));
    expect(chat.send).toHaveBeenCalledWith('Що тут не так?', true, [SHOT], undefined, 'phrase');
    expect(consume).toHaveBeenCalled();
    expect(api['mentorOwnerSpoke']).toHaveBeenCalledTimes(1);
  });

  it('sends a files-saved turn for a nudge only when idle', async () => {
    chat.busy.set(true);
    TestBed.tick();
    onNudge({ conversationId: 'c-1', files: ['src/app.ts'] });
    await Promise.resolve();
    expect(chat.send).not.toHaveBeenCalled();

    chat.busy.set(false);
    TestBed.tick();
    onNudge({ conversationId: 'other', files: ['src/app.ts'] });
    await Promise.resolve();
    expect(chat.send).not.toHaveBeenCalled();

    onNudge({ conversationId: 'c-1', files: ['src/app.ts', 'src/cart.ts'] });
    await vi.waitFor(() =>
      expect(chat.send).toHaveBeenCalledWith('[Saved: src/app.ts, src/cart.ts]', true, [], undefined, 'filesSaved')
    );
  });

  it('approves a medium write by voice but not an overwrite', async () => {
    approvals.set([approval('medium')]);
    await store.handlePhrase(phrase('Так'));
    expect(resolve).toHaveBeenCalledWith('a-1', true);
    expect(chat.send).not.toHaveBeenCalled();

    resolve.mockClear();
    approvals.set([approval('high')]);
    await store.handlePhrase(phrase('Так'));
    expect(resolve).not.toHaveBeenCalled();
    expect(store.notice()).toContain('click');
    await store.handlePhrase(phrase('Ні'));
    expect(resolve).toHaveBeenCalledWith('a-1', false);

    // Without a pending write, "yes" is just a phrase for the mentor.
    approvals.set([]);
    await store.handlePhrase(phrase('Так'));
    expect(chat.send).toHaveBeenCalledWith('Так', true, [], undefined, 'phrase');
  });

  it('continues a paused lesson by push-to-talk', async () => {
    onStatus({ ...LISTENING, phase: 'paused' });
    const push = { ...phrase('Продовжуємо'), source: 'pushToTalk' as const };
    expect(await store.handlePausedPhrase(push)).toBe(true);
    expect(api['mentorResume']).toHaveBeenCalled();
    expect(chat.send).not.toHaveBeenCalled();
    expect(await store.handlePausedPhrase({ ...push, text: 'Привіт' })).toBe(false);
  });

  it('asks to open the lesson conversation before sending', async () => {
    chat.conversation.set({ id: 'other' });
    await store.handlePhrase(phrase('Що далі?'));
    expect(chat.send).not.toHaveBeenCalled();
    expect(store.notice()).toContain('conversation');
  });
});

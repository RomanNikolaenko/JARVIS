import type { VoicePackage, VoiceState, VoiceStatus } from '@jarvis/shared-types';

export const IDLE_VOICE: VoiceState = {
  captureId: null,
  phase: 'idle',
  micOpen: false,
  wakeWordActive: false,
  awaitingCommand: false,
  hearingSpeech: false,
  inputDevice: null,
  lastError: null,
};

export function voicePackage(
  id: string,
  kind: VoicePackage['kind'],
  installed: boolean,
  extra: Partial<VoicePackage> = {}
): VoicePackage {
  return {
    id,
    kind,
    displayName: id,
    description: `${id} description`,
    language: null,
    source: 'github.com/example, release 1',
    sizeBytes: 10_000_000,
    license: 'MIT',
    speakers: [],
    recommended: false,
    installed,
    ...extra,
  };
}

/** Voice status with recognition ready and no voice installed. */
export function voiceStatus(extra: Partial<VoiceStatus> = {}): VoiceStatus {
  return {
    packages: [
      voicePackage('whisper.cpp', 'speechEngine', true, { recommended: true }),
      voicePackage('piper', 'voiceEngine', false, { recommended: true }),
      voicePackage('whisper-small-q5_1', 'speechModel', true, { recommended: true }),
      voicePackage('piper-uk', 'voice', false, { language: 'uk', speakers: ['Лада', 'Микита', 'Тетяна'], recommended: true }),
    ],
    downloads: [],
    inputDevices: [{ id: 'mic-1', name: 'USB microphone', isDefault: true }],
    outputDevices: [],
    deviceError: null,
    unsupported: null,
    canListen: true,
    canSpeak: false,
    state: IDLE_VOICE,
    voiceDir: '/data/voice',
    ...extra,
  };
}

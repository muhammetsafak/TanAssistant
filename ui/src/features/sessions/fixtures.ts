import type { ProcessingJob, Session, SessionDetail } from './api';

/** Test data shaped like the backend's answers. */
export const session: Session = {
  id: 's1',
  title: 'Weekly sync',
  kind: 'meeting',
  status: 'processed',
  startedAt: Date.UTC(2026, 10, 5, 9, 0),
  endedAt: Date.UTC(2026, 10, 5, 9, 45),
  durationMs: 45 * 60 * 1000,
  language: 'tr',
  noteLanguage: 'tr',
};

export const frames = [
  { id: 'f1', timestampMs: 10_000, path: 'frames/f1.png', description: 'Slide: Q3 plan' },
  { id: 'f2', timestampMs: 65_000, path: 'frames/f2.png', description: 'Slide: Q4 budget' },
  { id: 'f3', timestampMs: 300_000, path: 'frames/f3.png', description: null },
];

export const processedJob: ProcessingJob = {
  sessionId: 's1',
  steps: [
    { step: 'transcribe', state: 'done', error: null },
    { step: 'describe_frames', state: 'done', error: null },
    { step: 'synthesize', state: 'done', error: null },
  ],
  createdAt: 1,
  updatedAt: 2,
  finishedAt: 3,
};

export function sessionDetail(overrides: Partial<SessionDetail> = {}): SessionDetail {
  return {
    session,
    sessionDir: String.raw`C:\data\sessions\s1`,
    audioTracks: [
      {
        kind: 'microphone',
        path: 'audio/microphone.wav',
        sampleRate: 16000,
        durationMs: 2_700_000,
        deletedAt: null,
      },
    ],
    frames,
    transcript: [
      {
        track: 'microphone',
        startMs: 12_000,
        endMs: 15_000,
        language: 'tr',
        text: 'Merhaba herkese',
        speaker: 'me',
      },
      {
        track: 'system',
        startMs: 70_000,
        endMs: 75_000,
        language: 'tr',
        text: 'Bütçeye bakalım',
        speaker: 'others',
      },
    ],
    note: {
      id: 'n1',
      language: 'tr',
      summary: 'Q4 bütçesi konuşuldu.',
      decisions: ['Bütçe onaylandı'],
      openQuestions: ['Kim sunacak?'],
      keyFrameIds: ['f2'],
      createdAt: Date.UTC(2026, 10, 5, 10, 0),
      actionItems: [
        {
          id: 'a1',
          task: 'Bütçeyi paylaş',
          owner: 'Ayşe',
          due: Date.UTC(2026, 10, 12),
          done: false,
        },
        { id: 'a2', task: 'Sunumu hazırla', owner: null, due: null, done: true },
      ],
    },
    job: processedJob,
    ...overrides,
  };
}

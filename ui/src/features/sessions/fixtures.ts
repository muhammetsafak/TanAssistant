import type { ActionBrief, Person, ProcessingJob, Session, SessionDetail } from './api';

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

/** The person directory: the "me" person first, then by name. */
export const me: Person = { id: 'me', displayName: 'Muhammet', isSelf: true };
export const ayse: Person = { id: 'p1', displayName: 'Ayşe', isSelf: false };
export const mehmet: Person = { id: 'p2', displayName: 'Mehmet', isSelf: false };
export const caglar: Person = { id: 'p3', displayName: 'Çağlar', isSelf: false };
export const people: Person[] = [me, ayse, mehmet, caglar];

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
    trackMutes: [],
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
          ownerPersonId: 'p1',
          ownerName: 'Ayşe',
          ownerIsSelf: false,
          ownerEdited: false,
          due: Date.UTC(2026, 10, 12),
          done: false,
        },
        {
          id: 'a2',
          task: 'Sunumu hazırla',
          ownerPersonId: null,
          ownerName: null,
          ownerIsSelf: false,
          ownerEdited: false,
          due: null,
          done: true,
        },
      ],
    },
    participants: [],
    job: processedJob,
    ...overrides,
  };
}

/** The brief of action item `a1`. */
export const actionBrief: ActionBrief = {
  actionItemId: 'a1',
  language: 'tr',
  context: 'Q4 bütçesi toplantıda onaylandı; Ayşe rakamları ekiple paylaşacak.',
  details: ['Dosya PDF olarak gönderilecek', 'Cuma gününe kadar bitmeli'],
  relatedDecisions: ['Bütçe onaylandı'],
  openQuestions: ['Kim sunacak?'],
  frames: [
    { frameId: 'f2', reason: 'Bütçe tablosu bu karede' },
    { frameId: 'missing', reason: 'Artık olmayan bir kare' },
  ],
  quotes: [
    { atMs: 12_000, speaker: 'Ayşe', text: 'Rakamları ben paylaşırım' },
    { atMs: 70_000, speaker: 'others', text: 'Bütçeye bakalım' },
  ],
  suggestedSteps: ['Taslağı önce Mehmet ile gözden geçir', 'Paylaşım sonrası onay iste'],
  providerProfileId: 'prov1',
  createdAt: Date.UTC(2026, 10, 5, 11, 0),
};

import { vi } from 'vitest';

// Kept free of app imports: it is loaded from inside `vi.mock` factories of the bindings module.

type Handler = (event: { payload: unknown }) => void;

const EVENT_NAMES = [
  'processingProgressEvent',
  'recordingProgressEvent',
  'recordingStateEvent',
  'recordingWarningEvent',
  'transcriptionProgressEvent',
  'updateDownloadProgress',
  'whisperModelDownloadEvent',
] as const;

export type EventName = (typeof EVENT_NAMES)[number];

const handlers = new Map<string, Set<Handler>>();

/** A stand-in for the generated `events` object: `listen` registers a handler `emitEvent` can reach. */
export function stubEvents() {
  const entries = EVENT_NAMES.map((name) => [
    name,
    {
      listen: vi.fn((handler: Handler) => {
        const set = handlers.get(name) ?? new Set<Handler>();
        set.add(handler);
        handlers.set(name, set);
        return Promise.resolve(() => {
          set.delete(handler);
        });
      }),
    },
  ]);
  return Object.fromEntries(entries) as Record<
    EventName,
    { listen: (handler: Handler) => Promise<() => void> }
  >;
}

/** Delivers `payload` to everything listening to the stubbed event `name`. */
export function emitEvent(name: EventName, payload: unknown): void {
  for (const handler of handlers.get(name) ?? []) handler({ payload });
}

import { useSyncExternalStore } from 'react';

import type { TranscriptLine } from './api';

/**
 * Where the audio player is, in milliseconds. It lives outside React state so the player can
 * report its position several times a second without re-rendering the whole session page: only
 * the readers that derive something that actually changed (see `useActiveLineIndex`) render.
 */
export interface Playhead {
  get: () => number;
  set: (ms: number) => void;
  subscribe: (listener: () => void) => () => void;
}

/** The position before the player has been played or moved: no line is the one being played. */
export const NOWHERE = -1;

export function createPlayhead(initialMs = NOWHERE): Playhead {
  let position = initialMs;
  const listeners = new Set<() => void>();
  return {
    get: () => position,
    set: (ms) => {
      if (ms === position) return;
      position = ms;
      for (const listener of listeners) listener();
    },
    subscribe: (listener) => {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
  };
}

/** Stands in when no player is wired: it never moves, so nothing is highlighted ever changes. */
const IDLE_PLAYHEAD = createPlayhead(NOWHERE);

/**
 * The line being spoken at the playhead (`startMs <= position < endMs`), or `-1`. Lines of the two
 * tracks can overlap; the one that started last wins. The hook returns a number, so a component
 * using it renders only when the spoken line changes, not on every tick.
 */
export function useActiveLineIndex(
  playhead: Playhead | undefined,
  lines: readonly TranscriptLine[],
): number {
  const source = playhead ?? IDLE_PLAYHEAD;
  return useSyncExternalStore(source.subscribe, () => activeLineIndex(lines, source.get()));
}

export function activeLineIndex(lines: readonly TranscriptLine[], positionMs: number): number {
  let found = -1;
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    if (line === undefined) continue;
    if (
      line.startMs <= positionMs &&
      positionMs < line.endMs &&
      (found === -1 || line.startMs >= (lines[found]?.startMs ?? 0))
    ) {
      found = index;
    }
  }
  return found;
}

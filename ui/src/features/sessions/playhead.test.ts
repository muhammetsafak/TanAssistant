import { describe, expect, it, vi } from 'vitest';

import type { TranscriptLine } from './api';
import { activeLineIndex, createPlayhead, NOWHERE } from './playhead';

function line(track: string, startMs: number, endMs: number): TranscriptLine {
  return { track, startMs, endMs, language: 'tr', text: `${track}@${startMs}`, speaker: null };
}

describe('activeLineIndex', () => {
  const lines = [
    line('microphone', 0, 5000),
    line('system', 3000, 8000),
    line('microphone', 9000, 12000),
  ];

  it('finds the line being spoken, with the end of a line not belonging to it', () => {
    expect(activeLineIndex(lines, 0)).toBe(0);
    expect(activeLineIndex(lines, 4999)).toBe(1);
    expect(activeLineIndex(lines, 5000)).toBe(1);
    expect(activeLineIndex(lines, 8000)).toBe(-1);
    expect(activeLineIndex(lines, 9000)).toBe(2);
    expect(activeLineIndex(lines, 12000)).toBe(-1);
  });

  it('prefers the line that started last when two tracks overlap, whatever the order', () => {
    expect(activeLineIndex(lines, 3500)).toBe(1);
    expect(activeLineIndex([...lines].reverse(), 3500)).toBe(1);
  });

  it('marks nothing before the player has a position', () => {
    expect(activeLineIndex(lines, NOWHERE)).toBe(-1);
  });
});

describe('createPlayhead', () => {
  it('starts nowhere and tells listeners only about real changes', () => {
    const playhead = createPlayhead();
    const listener = vi.fn();
    const unsubscribe = playhead.subscribe(listener);
    expect(playhead.get()).toBe(NOWHERE);

    playhead.set(100);
    playhead.set(100);
    expect(listener).toHaveBeenCalledTimes(1);

    unsubscribe();
    playhead.set(200);
    expect(listener).toHaveBeenCalledTimes(1);
  });
});

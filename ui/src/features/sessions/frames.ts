import type { Frame } from './api';

/** The frame closest in time to `ms` (before or after it); `null` when there are no frames. */
export function nearestFrame(frames: readonly Frame[], ms: number): Frame | null {
  let best: Frame | null = null;
  let bestDistance = Infinity;
  for (const frame of frames) {
    const distance = Math.abs(frame.timestampMs - ms);
    if (distance < bestDistance) {
      best = frame;
      bestDistance = distance;
    }
  }
  return best;
}

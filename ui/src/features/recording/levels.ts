/**
 * RMS levels of speech sit low on a linear scale, so the bar follows the square root: quiet talk
 * still moves it. `null` means the source delivers nothing (no system audio on this platform).
 */
export function meterValue(level: number | null): number {
  if (level === null) return 0;
  return Math.min(1, Math.sqrt(Math.max(0, level)));
}

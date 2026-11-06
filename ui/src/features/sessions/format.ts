import type { TFunction } from 'i18next';

/** `m:ss`, or `h:mm:ss` from one hour on: for timers and transcript timestamps. */
export function formatClock(ms: number): string {
  const totalSeconds = Math.max(0, Math.floor(ms / 1000));
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  const two = (value: number) => value.toString().padStart(2, '0');
  return hours > 0
    ? `${hours}:${two(minutes)}:${two(seconds)}`
    : `${minutes.toString().padStart(2, '0')}:${two(seconds)}`;
}

/** A length of time in words ("1 sa 5 dk", "12 dk 30 sn"), for lists and forecasts. */
export function formatDuration(ms: number, t: TFunction): string {
  const totalSeconds = Math.max(0, Math.round(ms / 1000));
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  const parts: string[] = [];
  if (hours > 0) parts.push(`${hours} ${t('format.hours')}`);
  if (minutes > 0) parts.push(`${minutes} ${t('format.minutes')}`);
  // Seconds only matter for short recordings.
  if (hours === 0 && (seconds > 0 || parts.length === 0))
    parts.push(`${seconds} ${t('format.seconds')}`);
  return parts.join(' ');
}

/** A moment (milliseconds since the Unix epoch) in the user's language and local time. */
export function formatDateTime(ms: number | null, language: string): string {
  if (ms === null) return '';
  return new Intl.DateTimeFormat(language, { dateStyle: 'medium', timeStyle: 'short' }).format(ms);
}

/** A calendar day stored as midnight UTC (action item due dates) without a time-zone shift. */
export function formatUtcDate(ms: number, language: string): string {
  return new Intl.DateTimeFormat(language, { dateStyle: 'medium', timeZone: 'UTC' }).format(ms);
}

/** A byte count with a binary unit ("1,4 GB"); `null` (unknown) is an em dash. */
export function formatBytes(bytes: number | null, language: string): string {
  if (bytes === null) return '—';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let value = Math.max(0, bytes);
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const digits = unit === 0 ? 0 : 1;
  return `${new Intl.NumberFormat(language, { maximumFractionDigits: digits }).format(value)} ${units[unit]}`;
}

/** A file name for the save dialog: the title without characters file systems refuse. */
export function suggestedFileName(title: string): string {
  // Control characters (below U+0020) are refused too; the whitespace collapse below absorbs them.
  const printable = Array.from(title, (char) => (char.charCodeAt(0) < 32 ? ' ' : char)).join('');
  const cleaned = printable
    .replace(/[<>:"/\\|?*]/g, ' ')
    .replace(/\s+/g, ' ')
    .trim()
    .replace(/[. ]+$/, '');
  return `${cleaned.slice(0, 80).trim() || 'notes'}.md`;
}

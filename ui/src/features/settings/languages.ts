/** Languages offered for speech and notes. Whisper and the AI providers handle them all. */
export const LANGUAGE_CODES = [
  'tr',
  'en',
  'de',
  'fr',
  'es',
  'it',
  'pt',
  'nl',
  'ru',
  'ar',
  'ja',
  'zh',
] as const;

/** The speech-language setting that lets the engine detect the language. */
export const AUTO_LANGUAGE = 'auto';

/** The offered codes, plus `current` when a stored value is outside the list (never hidden). */
export function languageOptions(current: string | null | undefined, withAuto: boolean): string[] {
  const codes: string[] = withAuto ? [AUTO_LANGUAGE, ...LANGUAGE_CODES] : [...LANGUAGE_CODES];
  if (current && !codes.includes(current)) codes.push(current);
  return codes;
}

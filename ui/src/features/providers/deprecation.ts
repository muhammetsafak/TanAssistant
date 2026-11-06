import { useTranslation } from 'react-i18next';

import type { ProviderKind, ProviderKindInfo } from './api';

export interface DeprecationNotice {
  /** The day the provider shuts the model down (`YYYY-MM-DD`). */
  shutdownDate: string;
  replacement: string | null;
  /** Whole days from today to the shutdown day; negative once it has passed. */
  daysLeft: number;
}

const DAY_MS = 24 * 60 * 60 * 1000;

/** Whole days from `today` (local calendar day) to the `YYYY-MM-DD` day `date`; `null` if malformed. */
export function daysUntil(date: string, today: Date = new Date()): number | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(date);
  if (!match) return null;
  const target = Date.UTC(Number(match[1]), Number(match[2]) - 1, Number(match[3]));
  const start = Date.UTC(today.getFullYear(), today.getMonth(), today.getDate());
  return Math.round((target - start) / DAY_MS);
}

/** The shutdown announcement for `model` of `kind`, when its provider has made one. */
export function deprecationFor(
  kinds: readonly ProviderKindInfo[] | undefined,
  kind: ProviderKind,
  model: string,
): DeprecationNotice | null {
  const entry = kinds
    ?.find((info) => info.kind === kind)
    ?.deprecatedModels?.find((candidate) => candidate.model === model.trim());
  if (!entry) return null;
  const daysLeft = daysUntil(entry.shutdownDate);
  if (daysLeft === null) return null;
  return { shutdownDate: entry.shutdownDate, replacement: entry.replacement, daysLeft };
}

/** `YYYY-MM-DD` as a calendar day in the UI language (no time-zone shift). */
function formatShutdownDate(date: string, language: string): string {
  const [year = 0, month = 1, day = 1] = date.split('-').map(Number);
  return new Intl.DateTimeFormat(language, { dateStyle: 'long', timeZone: 'UTC' }).format(
    Date.UTC(year, month - 1, day),
  );
}

/** The dated warning text for a notice, e.g. "Closes on 1 March 2027 (in 115 days)". */
export function useDeprecationText(notice: DeprecationNotice | null): string | null {
  const { t, i18n } = useTranslation();
  if (!notice) return null;
  const date = formatShutdownDate(notice.shutdownDate, i18n.language);
  const key =
    notice.daysLeft < 0
      ? 'providers.deprecation.closed'
      : notice.daysLeft === 0
        ? 'providers.deprecation.closesToday'
        : 'providers.deprecation.closes';
  return t(key, { date, count: notice.daysLeft });
}

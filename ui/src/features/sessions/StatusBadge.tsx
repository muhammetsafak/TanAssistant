import { useTranslation } from 'react-i18next';

import type { SessionStatus } from './api';

import './sessions.css';

/** The session status as a labelled badge: the colour backs up the text, never replaces it. */
export function StatusBadge({ status }: { status: SessionStatus }) {
  const { t } = useTranslation();
  return (
    <span className={`status-badge status-badge--${status}`}>{t(`sessions.status.${status}`)}</span>
  );
}

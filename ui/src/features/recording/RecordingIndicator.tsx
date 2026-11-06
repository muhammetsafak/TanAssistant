import { useTranslation } from 'react-i18next';
import { Link } from 'react-router';

import { formatClock } from '../sessions/format';
import { useRecordingStatus } from './api';
import { useRecordingEvents } from './useRecordingEvents';

import './recording.css';

/** Top-bar link that shows a recording is running, wherever the user is in the app. */
export function RecordingIndicator() {
  const { t } = useTranslation();
  const status = useRecordingStatus();
  const { live } = useRecordingEvents();
  const active = status.data?.state === 'recording' ? status.data.active : null;
  if (!active) return null;

  const elapsedMs = live?.sessionId === active.sessionId ? live.elapsedMs : active.elapsedMs;
  return (
    <Link to="/recordings" className="recording-indicator">
      <span className="recording-dot" aria-hidden="true" />
      <span>{t('recording.indicator')}</span>
      <span className="recording-indicator__time">{formatClock(elapsedMs)}</span>
    </Link>
  );
}

import { useTranslation } from 'react-i18next';
import { Link } from 'react-router';

import { formatClock } from '../sessions/format';
import { useRecordingStatus } from './api';
import { PauseGlyph } from './PauseGlyph';
import { useRecordingEvents } from './useRecordingEvents';

import './recording.css';

/** Top-bar link that shows a recording is running, wherever the user is in the app. */
export function RecordingIndicator() {
  const { t } = useTranslation();
  const status = useRecordingStatus();
  const { live } = useRecordingEvents();
  const state = status.data?.state;
  const active = state === 'recording' || state === 'paused' ? status.data?.active : null;
  if (!active) return null;

  const paused = state === 'paused';
  // A paused clock stands still: only a running recording follows the progress ticks.
  const elapsedMs =
    !paused && live?.sessionId === active.sessionId ? live.elapsedMs : active.elapsedMs;
  return (
    <Link
      to="/recordings"
      className={paused ? 'recording-indicator recording-indicator--paused' : 'recording-indicator'}
    >
      {paused ? <PauseGlyph /> : <span className="recording-dot" aria-hidden="true" />}
      <span>{paused ? t('recording.indicatorPaused') : t('recording.indicator')}</span>
      <span className="recording-indicator__time">{formatClock(elapsedMs)}</span>
    </Link>
  );
}

import { useTranslation } from 'react-i18next';

import { meterValue } from './levels';

import './recording.css';

const BAR_COUNT = 16;

export interface LevelMeterProps {
  /** Who is speaking on this channel ("Me", "Other side"). */
  name: string;
  /** What feeds it ("Microphone", "System audio"). */
  source: string;
  /** Which colour the lit bars take: the user's own voice or the other side. */
  tone: 'me' | 'them';
  level: number | null;
  /**
   * `live` follows the level, `paused` is empty and says so, `idle` is the dimmed preview shown
   * before a recording starts (decorative: there is nothing to measure yet).
   */
  mode?: 'live' | 'paused' | 'idle';
}

/** One channel's level: a row of bars, backed by a real `<meter>` for assistive technology. */
export function LevelMeter({ name, source, tone, level, mode = 'live' }: LevelMeterProps) {
  const { t } = useTranslation();
  const measuring = mode === 'live';
  const value = measuring ? meterValue(level) : 0;
  const lit = Math.round(value * BAR_COUNT);
  const label = t('recording.meterLabel', { name: source });

  return (
    <div className={`level level--${tone}${mode === 'idle' ? ' level--idle' : ''}`}>
      <div className="level__header">
        <span className="level__name">{name}</span>
        <span className="level__source">{source}</span>
      </div>
      <div className="level__bars" aria-hidden="true">
        {/* The height of a lit bar (an equaliser profile per channel) comes from recording.css. */}
        {Array.from({ length: BAR_COUNT }, (_, index) => (
          <span key={index} className={index < lit ? 'level__bar level__bar--on' : 'level__bar'} />
        ))}
      </div>
      {mode === 'idle' ? null : (
        <meter className="sr-only" min={0} max={1} value={value} aria-label={label} />
      )}
      {mode === 'paused' ? (
        <span className="level__idle">{t('recording.meterPaused')}</span>
      ) : mode === 'live' && level === null ? (
        <span className="level__idle">{t('recording.noSignal')}</span>
      ) : null}
    </div>
  );
}

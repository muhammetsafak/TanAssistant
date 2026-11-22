import type { ReactNode } from 'react';
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
  /** The channel is muted: its bars stay empty and the meter says it is off. */
  muted?: boolean;
  /** A control that belongs to this channel (its mute button), shown under the bars. */
  control?: ReactNode;
}

/** One channel's level: a row of bars, backed by a real `<meter>` for assistive technology. */
export function LevelMeter({
  name,
  source,
  tone,
  level,
  mode = 'live',
  muted = false,
  control,
}: LevelMeterProps) {
  const { t } = useTranslation();
  const off = muted && mode !== 'idle';
  const measuring = mode === 'live' && !off;
  const value = measuring ? meterValue(level) : 0;
  const lit = Math.round(value * BAR_COUNT);
  const label = t('recording.meterLabel', { name: source });
  const caption = off
    ? t('recording.meterMuted')
    : mode === 'paused'
      ? t('recording.meterPaused')
      : mode === 'live' && level === null
        ? t('recording.noSignal')
        : null;

  return (
    <div
      className={`level level--${tone}${mode === 'idle' ? ' level--idle' : ''}${off ? ' level--muted' : ''}`}
    >
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
      {caption || control ? (
        <div className="level__footer">
          {caption ? <span className="level__idle">{caption}</span> : null}
          {control}
        </div>
      ) : null}
    </div>
  );
}

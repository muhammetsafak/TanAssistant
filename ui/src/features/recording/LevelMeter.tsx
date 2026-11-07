import { useTranslation } from 'react-i18next';

import { meterValue } from './levels';

import './recording.css';

export function LevelMeter({
  label,
  level,
  paused = false,
}: {
  label: string;
  level: number | null;
  /** A paused recording captures nothing: the bar is empty and says so. */
  paused?: boolean;
}) {
  const { t } = useTranslation();
  return (
    <div className="level">
      <span className="level__label">{label}</span>
      <meter
        className="level__meter"
        min={0}
        max={1}
        value={paused ? 0 : meterValue(level)}
        aria-label={t('recording.meterLabel', { name: label })}
      />
      {paused ? (
        <span className="level__idle">{t('recording.meterPaused')}</span>
      ) : level === null ? (
        <span className="level__idle">{t('recording.noSignal')}</span>
      ) : null}
    </div>
  );
}

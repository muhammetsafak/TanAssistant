import { useTranslation } from 'react-i18next';

import { meterValue } from './levels';

import './recording.css';

export function LevelMeter({ label, level }: { label: string; level: number | null }) {
  const { t } = useTranslation();
  return (
    <div className="level">
      <span className="level__label">{label}</span>
      <meter
        className="level__meter"
        min={0}
        max={1}
        value={meterValue(level)}
        aria-label={t('recording.meterLabel', { name: label })}
      />
      {level === null ? <span className="level__idle">{t('recording.noSignal')}</span> : null}
    </div>
  );
}

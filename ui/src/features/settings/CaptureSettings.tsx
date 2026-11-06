import { useState, type FormEvent } from 'react';
import { useTranslation } from 'react-i18next';

import { Button } from '../../components/ui/Button';
import { TextField } from '../../components/ui/TextField';
import { useToast } from '../../components/ui/toast-context';
import type { AppSettings } from '../../ipc/client';
import { useSettings, useUpdateSettings } from './useSettings';

import '../../components/ui/ui.css';
import './settings.css';

// Mirrors the limits of the backend settings, so a bad value is caught before the call.
const MIN_INTERVAL_SECONDS = 0.5;
const MAX_INTERVAL_SECONDS = 60;
const MAX_THRESHOLD = 64;

/** Frame sampling and the processing behaviour after a recording. */
export function CaptureSettings() {
  const { t } = useTranslation();
  const settings = useSettings();

  return (
    <section className="settings-section" aria-labelledby="settings-capture-heading">
      <div>
        <h2 className="settings-section__title" id="settings-capture-heading">
          {t('settings.capture.title')}
        </h2>
        <p className="settings-section__hint">{t('settings.capture.hint')}</p>
      </div>
      {settings.isPending ? (
        <p role="status">{t('common.loading')}</p>
      ) : settings.isError ? (
        <div className="alert" role="alert">
          <span>{t('errors.settingsLoad')}</span>
          <Button size="sm" onClick={() => void settings.refetch()}>
            {t('common.retry')}
          </Button>
        </div>
      ) : (
        // Re-created when the stored values change, so the fields always start from them.
        <CaptureForm
          key={`${settings.data.frameIntervalMs}|${settings.data.frameChangeThreshold}`}
          settings={settings.data}
        />
      )}
    </section>
  );
}

function CaptureForm({ settings }: { settings: AppSettings }) {
  const { t } = useTranslation();
  const toast = useToast();
  const update = useUpdateSettings();
  const [seconds, setSeconds] = useState(String(settings.frameIntervalMs / 1000));
  const [threshold, setThreshold] = useState(String(settings.frameChangeThreshold));
  const [errors, setErrors] = useState<{ interval?: string; threshold?: string }>({});

  function submit(event: FormEvent) {
    event.preventDefault();
    const intervalValue = Number(seconds.replace(',', '.'));
    const thresholdValue = Number(threshold);
    const next = {
      interval:
        seconds.trim() !== '' &&
        Number.isFinite(intervalValue) &&
        intervalValue >= MIN_INTERVAL_SECONDS &&
        intervalValue <= MAX_INTERVAL_SECONDS
          ? undefined
          : t('settings.capture.intervalInvalid', {
              min: MIN_INTERVAL_SECONDS,
              max: MAX_INTERVAL_SECONDS,
            }),
      threshold:
        threshold.trim() !== '' &&
        Number.isInteger(thresholdValue) &&
        thresholdValue >= 0 &&
        thresholdValue <= MAX_THRESHOLD
          ? undefined
          : t('settings.capture.thresholdInvalid', { max: MAX_THRESHOLD }),
    };
    setErrors(next);
    if (next.interval || next.threshold) return;

    update.mutate(
      {
        frameIntervalMs: Math.round(intervalValue * 1000),
        frameChangeThreshold: thresholdValue,
      },
      {
        onSuccess: () => toast.success(t('settings.saved')),
        onError: (error) => toast.error(error),
      },
    );
  }

  return (
    <>
      <form className="settings-form" onSubmit={submit} noValidate>
        <div className="settings-grid">
          <TextField
            type="number"
            inputMode="decimal"
            label={t('settings.capture.interval')}
            hint={t('settings.capture.intervalHint')}
            value={seconds}
            min={MIN_INTERVAL_SECONDS}
            max={MAX_INTERVAL_SECONDS}
            step={0.5}
            onChange={(event) => setSeconds(event.target.value)}
            error={errors.interval}
          />
          <TextField
            type="number"
            inputMode="numeric"
            label={t('settings.capture.threshold')}
            hint={t('settings.capture.thresholdHint', { max: MAX_THRESHOLD })}
            value={threshold}
            min={0}
            max={MAX_THRESHOLD}
            step={1}
            onChange={(event) => setThreshold(event.target.value)}
            error={errors.threshold}
          />
        </div>
        <div>
          <Button type="submit" variant="primary" disabled={update.isPending}>
            {t('common.save')}
          </Button>
        </div>
      </form>

      <div className="field">
        <label className="settings-check">
          <input
            type="checkbox"
            checked={settings.autoProcessAfterRecording}
            disabled={update.isPending}
            onChange={(event) =>
              update.mutate(
                { autoProcessAfterRecording: event.target.checked },
                {
                  onSuccess: () => toast.success(t('settings.saved')),
                  onError: (error) => toast.error(error),
                },
              )
            }
          />
          <span>{t('settings.capture.autoProcess')}</span>
        </label>
        <p className="field__hint">{t('settings.capture.autoProcessHint')}</p>
      </div>
    </>
  );
}

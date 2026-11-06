import { useState, type FormEvent } from 'react';
import { useTranslation } from 'react-i18next';

import { Button } from '../../components/ui/Button';
import { TextField } from '../../components/ui/TextField';
import { useToast } from '../../components/ui/toast-context';
import { formatBytes } from '../sessions/format';
import { useStorageUsage } from './api';
import { useSettings, useUpdateSettings } from './useSettings';

import '../../components/ui/ui.css';
import './settings.css';

/** How long audio files are kept after processing, and how much disk Tan's data uses. */
export function StorageSettings() {
  const { t } = useTranslation();
  const settings = useSettings();

  return (
    <section className="settings-section" aria-labelledby="settings-storage-heading">
      <div>
        <h2 className="settings-section__title" id="settings-storage-heading">
          {t('settings.storage.title')}
        </h2>
        <p className="settings-section__hint">{t('settings.storage.hint')}</p>
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
        <RetentionForm
          key={String(settings.data.audioRetentionDays)}
          storedDays={settings.data.audioRetentionDays}
        />
      )}

      <DiskUsage />
    </section>
  );
}

function RetentionForm({ storedDays }: { storedDays: number | null }) {
  const { t } = useTranslation();
  const toast = useToast();
  const update = useUpdateSettings();
  const [keepForever, setKeepForever] = useState(storedDays === null);
  const [days, setDays] = useState(String(storedDays ?? 30));
  const [error, setError] = useState<string | null>(null);

  function submit(event: FormEvent) {
    event.preventDefault();
    const value = Number(days);
    if (!keepForever && !(days.trim() !== '' && Number.isInteger(value) && value >= 1)) {
      setError(t('settings.storage.daysInvalid'));
      return;
    }
    setError(null);
    update.mutate(
      { audioRetention: { days: keepForever ? null : value } },
      {
        onSuccess: () => toast.success(t('settings.saved')),
        onError: (failure) => toast.error(failure),
      },
    );
  }

  return (
    <form className="settings-form" onSubmit={submit} noValidate>
      <fieldset className="settings-fieldset">
        <legend className="field__label">{t('settings.storage.retention')}</legend>
        <label className="settings-check">
          <input
            type="radio"
            name="audio-retention"
            checked={keepForever}
            onChange={() => setKeepForever(true)}
          />
          <span>{t('settings.storage.keepForever')}</span>
        </label>
        <label className="settings-check">
          <input
            type="radio"
            name="audio-retention"
            checked={!keepForever}
            onChange={() => setKeepForever(false)}
          />
          <span>{t('settings.storage.deleteAfter')}</span>
        </label>
        {!keepForever ? (
          <TextField
            type="number"
            inputMode="numeric"
            label={t('settings.storage.days')}
            hint={t('settings.storage.daysHint')}
            value={days}
            min={1}
            step={1}
            onChange={(event) => setDays(event.target.value)}
            error={error}
          />
        ) : null}
      </fieldset>
      <div>
        <Button type="submit" variant="primary" disabled={update.isPending}>
          {t('common.save')}
        </Button>
      </div>
    </form>
  );
}

function DiskUsage() {
  const { t, i18n } = useTranslation();
  const usage = useStorageUsage();
  const language = i18n.language;

  return (
    <div className="disk-usage">
      <div className="settings-section__row">
        <h3 className="disk-usage__title">{t('settings.storage.usage')}</h3>
        <Button size="sm" onClick={() => void usage.refetch()} disabled={usage.isFetching}>
          {t('settings.storage.refresh')}
        </Button>
      </div>
      {usage.isPending ? (
        <p role="status">{t('common.loading')}</p>
      ) : usage.isError ? (
        <div className="alert" role="alert">
          <span>{t('settings.storage.usageFailed')}</span>
          <Button size="sm" onClick={() => void usage.refetch()}>
            {t('common.retry')}
          </Button>
        </div>
      ) : (
        <dl className="disk-usage__list">
          <div>
            <dt>{t('settings.storage.total')}</dt>
            <dd>{formatBytes(usage.data.totalBytes, language)}</dd>
          </div>
          <div>
            <dt>{t('settings.storage.audio')}</dt>
            <dd>{formatBytes(usage.data.audioBytes, language)}</dd>
          </div>
          <div>
            <dt>{t('settings.storage.frames')}</dt>
            <dd>{formatBytes(usage.data.frameBytes, language)}</dd>
          </div>
          <div>
            <dt>{t('settings.storage.models')}</dt>
            <dd>{formatBytes(usage.data.modelBytes, language)}</dd>
          </div>
        </dl>
      )}
    </div>
  );
}

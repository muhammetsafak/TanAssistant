import { useState } from 'react';
import { useTranslation } from 'react-i18next';

import { Button } from '../../components/ui/Button';
import { ConfirmDialog } from '../../components/ui/ConfirmDialog';
import { useToast } from '../../components/ui/toast-context';
import { useCommandErrorHandler } from '../../ipc/errors';
import { formatBytes } from '../sessions/format';
import {
  useDeleteWhisperModel,
  useDownloadingModelIds,
  useDownloadProgress,
  useDownloadWhisperModel,
  useSelectWhisperModel,
  useWhisperModels,
  type DownloadProgress,
  type WhisperModel,
} from './api';

import '../../components/ui/ui.css';
import './settings.css';

/** The local whisper models: download with progress, choose the one in use, delete. */
export function WhisperModels() {
  const { t } = useTranslation();
  const handleError = useCommandErrorHandler();
  const toast = useToast();
  const models = useWhisperModels();
  const downloading = useDownloadingModelIds();
  const progress = useDownloadProgress();
  const download = useDownloadWhisperModel();
  const select = useSelectWhisperModel();
  const remove = useDeleteWhisperModel();
  const [deleting, setDeleting] = useState<WhisperModel | null>(null);

  function startDownload(model: WhisperModel) {
    download.mutate(model.id, {
      onSuccess: () => toast.success(t('settings.whisper.downloaded', { name: model.displayName })),
      onError: handleError,
    });
  }

  function choose(model: WhisperModel) {
    select.mutate(model.id, {
      onSuccess: () => toast.success(t('settings.whisper.selected', { name: model.displayName })),
      onError: handleError,
    });
  }

  function confirmDelete() {
    if (!deleting) return;
    const model = deleting;
    remove.mutate(model.id, {
      onSuccess: () => {
        toast.success(t('settings.whisper.deleted', { name: model.displayName }));
        setDeleting(null);
      },
      onError: (error) => {
        setDeleting(null);
        handleError(error);
      },
    });
  }

  return (
    <section className="settings-section" aria-labelledby="settings-whisper-heading">
      <div>
        <h2 className="settings-section__title" id="settings-whisper-heading">
          {t('settings.whisper.title')}
        </h2>
        <p className="settings-section__hint">{t('settings.whisper.hint')}</p>
      </div>

      {models.isPending ? (
        <p role="status">{t('common.loading')}</p>
      ) : models.isError ? (
        <div className="alert" role="alert">
          <span>{t('settings.whisper.loadFailed')}</span>
          <Button size="sm" onClick={() => void models.refetch()}>
            {t('common.retry')}
          </Button>
        </div>
      ) : models.data.length === 0 ? (
        <p className="settings-empty">{t('settings.whisper.empty')}</p>
      ) : (
        <ul className="model-list">
          {models.data.map((model) => (
            <ModelRow
              key={model.id}
              model={model}
              downloading={downloading.includes(model.id)}
              progress={progress[model.id]}
              busy={select.isPending || remove.isPending}
              onDownload={() => startDownload(model)}
              onSelect={() => choose(model)}
              onDelete={() => setDeleting(model)}
            />
          ))}
        </ul>
      )}

      <ConfirmDialog
        open={deleting !== null}
        title={t('settings.whisper.deleteTitle')}
        message={t('settings.whisper.deleteMessage', { name: deleting?.displayName ?? '' })}
        confirmLabel={t('common.delete')}
        pending={remove.isPending}
        onConfirm={confirmDelete}
        onCancel={() => setDeleting(null)}
      />
    </section>
  );
}

function ModelRow({
  model,
  downloading,
  progress,
  busy,
  onDownload,
  onSelect,
  onDelete,
}: {
  model: WhisperModel;
  downloading: boolean;
  progress: DownloadProgress | undefined;
  busy: boolean;
  onDownload: () => void;
  onSelect: () => void;
  onDelete: () => void;
}) {
  const { t, i18n } = useTranslation();
  const language = i18n.language;
  const percent =
    progress && progress.total > 0
      ? Math.min(100, Math.round((progress.downloaded / progress.total) * 100))
      : null;

  return (
    <li className="model-row">
      <div className="model-row__info">
        <p className="model-row__name">{model.displayName}</p>
        <p className="model-row__meta">
          {formatBytes(model.sizeBytes, language)} · {model.quantization}
        </p>
        <ul className="badge-row">
          {model.recommended ? (
            <li className="badge">{t('settings.whisper.recommended')}</li>
          ) : null}
          {model.installed ? <li className="badge">{t('settings.whisper.installed')}</li> : null}
          {model.selected ? (
            <li className="badge badge--active">{t('settings.whisper.inUse')}</li>
          ) : null}
        </ul>
        {!model.installed && !downloading && model.partialBytes > 0 ? (
          <p className="model-row__meta">
            {t('settings.whisper.partial', { size: formatBytes(model.partialBytes, language) })}
          </p>
        ) : null}
        {downloading ? (
          <div className="model-row__progress">
            <progress
              max={100}
              value={percent ?? undefined}
              aria-label={t('settings.whisper.downloadLabel', { name: model.displayName })}
            />
            <p className="model-row__meta" role="status">
              {progress
                ? t('settings.whisper.downloadProgress', {
                    downloaded: formatBytes(progress.downloaded, language),
                    total: formatBytes(progress.total, language),
                    percent: percent ?? 0,
                  })
                : t('settings.whisper.downloadStarting')}
            </p>
          </div>
        ) : null}
      </div>
      <div className="model-row__actions">
        {model.installed ? (
          <>
            {!model.selected ? (
              <Button
                size="sm"
                onClick={onSelect}
                disabled={busy}
                aria-label={t('settings.whisper.useModel', { name: model.displayName })}
              >
                {t('settings.whisper.use')}
              </Button>
            ) : null}
            <Button
              size="sm"
              onClick={onDelete}
              disabled={busy}
              aria-label={t('settings.whisper.deleteModel', { name: model.displayName })}
            >
              {t('common.delete')}
            </Button>
          </>
        ) : (
          <>
            <Button
              size="sm"
              variant="primary"
              onClick={onDownload}
              disabled={downloading}
              aria-label={t('settings.whisper.downloadModel', { name: model.displayName })}
            >
              {model.partialBytes > 0
                ? t('settings.whisper.resume')
                : t('settings.whisper.download')}
            </Button>
            {model.partialBytes > 0 && !downloading ? (
              <Button
                size="sm"
                onClick={onDelete}
                disabled={busy}
                aria-label={t('settings.whisper.deletePartial', { name: model.displayName })}
              >
                {t('common.delete')}
              </Button>
            ) : null}
          </>
        )}
      </div>
    </li>
  );
}

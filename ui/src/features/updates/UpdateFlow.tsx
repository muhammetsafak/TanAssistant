import { useId, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';

import { Button } from '../../components/ui/Button';
import { WarningIcon } from '../../components/ui/Icons';
import { describeError } from '../../ipc/client';
import { formatBytes } from '../sessions/format';
import {
  useRestartBlocked,
  useUpdateActions,
  useUpdateState,
  type UpdateProgress,
  type UpdateState,
} from './api';

import '../../components/ui/ui.css';
import './updates.css';

function percentOf(progress: UpdateProgress | null): number | null {
  if (!progress || progress.total === null || progress.total <= 0) return null;
  return Math.min(100, Math.round((progress.downloaded / progress.total) * 100));
}

/** One sentence naming where the flow stands; `null` when there is nothing to say yet. */
function statusKey(state: UpdateState): string | null {
  const { info, phase } = state;
  if (phase === 'checking') return 'updates.status.checking';
  if (phase === 'downloading') return 'updates.status.downloading';
  if (phase === 'ready') return 'updates.status.ready';
  if (phase === 'installing') return 'updates.status.installing';
  if (phase === 'restartPending') return 'updates.status.restartPending';
  if (!info) return null;
  if (!info.available) return 'updates.status.upToDate';
  return info.canSelfUpdate ? 'updates.status.available' : 'updates.status.availableManual';
}

/**
 * The update's status, the download progress and the button the current phase calls for. Both the
 * notification and the settings card show it; `children` are extra buttons of the host, placed in
 * the same row.
 */
export function UpdateFlow({ children }: { children?: ReactNode }) {
  const { t, i18n } = useTranslation();
  const state = useUpdateState();
  const actions = useUpdateActions();
  const { info, phase, progress, failure } = state;

  const key = statusKey(state);
  const percent = percentOf(progress);
  const canSelfUpdate = info?.available === true && info.canSelfUpdate;

  return (
    <div className="update-flow">
      {key ? (
        <p className="update-flow__status" role="status">
          {t(key, { version: info?.version ?? '', current: info?.currentVersion ?? '' })}
        </p>
      ) : null}

      {phase === 'downloading' ? (
        <div className="update-flow__progress">
          <progress
            max={100}
            value={percent ?? undefined}
            aria-label={t('updates.progressLabel')}
          />
          <p className="update-flow__meta">
            {percent !== null
              ? t('updates.progressPercent', { percent })
              : progress
                ? formatBytes(progress.downloaded, i18n.language)
                : t('updates.progressStarting')}
          </p>
        </div>
      ) : null}

      {/* A failure is also announced by a toast; this line keeps it visible in the card. */}
      {failure ? (
        <p className="field__error">
          <WarningIcon className="field__error-icon" />
          {describeError(failure.error, t).message}
        </p>
      ) : null}

      <div className="update-flow__actions">
        {info?.available === true && !info.canSelfUpdate ? (
          <Button variant="primary" onClick={() => void actions.openReleasePage()}>
            {t('updates.openRelease')}
          </Button>
        ) : null}
        {canSelfUpdate && phase === 'idle' ? (
          <Button variant="primary" onClick={() => void actions.download()}>
            {t('updates.download')}
          </Button>
        ) : null}
        {canSelfUpdate &&
        (phase === 'ready' || phase === 'installing' || phase === 'restartPending') ? (
          <RestartButton installing={phase === 'installing'} onRestart={actions.install} />
        ) : null}
        {children}
      </div>
    </div>
  );
}

/** "Restart" is off while a recording or a processing run is going on, and says why. */
function RestartButton({
  installing,
  onRestart,
}: {
  installing: boolean;
  onRestart: () => Promise<void>;
}) {
  const { t } = useTranslation();
  const blocked = useRestartBlocked();
  const noteId = useId();

  return (
    <>
      <Button
        variant="primary"
        disabled={blocked || installing}
        aria-describedby={blocked ? noteId : undefined}
        onClick={() => void onRestart()}
      >
        {t('updates.restart')}
      </Button>
      {blocked ? (
        <p className="update-flow__meta update-flow__note" id={noteId}>
          {t('updates.restartBlocked')}
        </p>
      ) : null}
    </>
  );
}

import { useQuery } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';

import { Button } from '../../components/ui/Button';
import { useToast } from '../../components/ui/toast-context';
import { ipc } from '../../ipc/client';
import { useSettings, useUpdateSettings } from '../settings/useSettings';
import { useUpdateActions, useUpdateState } from './api';
import { UpdateFlow } from './UpdateFlow';

import '../../components/ui/ui.css';
import '../settings/settings.css';
import './updates.css';

/** Update preferences: the running version, the automatic check, a manual check and its result. */
export function UpdateSettings() {
  const { t } = useTranslation();
  const toast = useToast();
  const settings = useSettings();
  const update = useUpdateSettings();
  const { info, phase } = useUpdateState();
  const actions = useUpdateActions();
  const { data: appVersion } = useQuery({ queryKey: ['app-version'], queryFn: ipc.appVersion });

  const version = info?.currentVersion ?? appVersion;

  return (
    <section className="settings-section" aria-labelledby="settings-updates-heading">
      <div>
        <h2 className="settings-section__title" id="settings-updates-heading">
          {t('updates.title')}
        </h2>
        <p className="settings-section__hint">{t('updates.hint')}</p>
      </div>

      {version ? (
        <p className="update-settings__version">{t('updates.currentVersion', { version })}</p>
      ) : null}

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
        <div className="field">
          <label className="settings-check">
            <input
              type="checkbox"
              checked={settings.data.autoCheckUpdates}
              disabled={update.isPending}
              onChange={(event) =>
                update.mutate(
                  { autoCheckUpdates: event.target.checked },
                  {
                    onSuccess: () => toast.success(t('settings.saved')),
                    onError: (error) => toast.error(error),
                  },
                )
              }
            />
            <span>{t('updates.autoCheck')}</span>
          </label>
          <p className="field__hint">{t('updates.autoCheckHint')}</p>
        </div>
      )}

      <UpdateFlow>
        <Button disabled={phase !== 'idle'} onClick={() => void actions.check(true)}>
          {phase === 'checking' ? t('updates.checking') : t('updates.checkNow')}
        </Button>
      </UpdateFlow>
    </section>
  );
}

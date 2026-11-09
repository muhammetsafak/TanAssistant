import { useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';

import { Button } from '../../components/ui/Button';
import { useSettings } from '../settings/useSettings';
import {
  UPDATE_CHECK_INTERVAL_MS,
  useUpdateActions,
  useUpdateProgressEvents,
  useUpdateState,
} from './api';
import { UpdateFlow } from './UpdateFlow';

import './updates.css';

/**
 * Checks for a new version when the app opens and then once a day (while "check automatically" is
 * on), and keeps a dismissible notification up while one is available: download, then restart.
 * The automatic check fails silently.
 */
export function UpdateNotifier() {
  const { t } = useTranslation();
  const settings = useSettings();
  const { info, dismissedVersions } = useUpdateState();
  const actions = useUpdateActions();
  useUpdateProgressEvents();

  const enabled = settings.data?.autoCheckUpdates === true;
  const checkRef = useRef(actions.check);
  useEffect(() => {
    checkRef.current = actions.check;
  });

  useEffect(() => {
    if (!enabled) return;
    void checkRef.current(false);
    const timer = setInterval(() => void checkRef.current(false), UPDATE_CHECK_INTERVAL_MS);
    return () => clearInterval(timer);
  }, [enabled]);

  if (!info?.available || !info.version || dismissedVersions.includes(info.version)) return null;
  const version = info.version;

  return (
    <section className="update-banner" aria-label={t('updates.bannerLabel')}>
      <h2 className="update-banner__title">{t('updates.bannerTitle', { version })}</h2>
      <UpdateFlow>
        <Button variant="ghost" onClick={() => actions.dismiss(version)}>
          {t('updates.dismiss')}
        </Button>
      </UpdateFlow>
    </section>
  );
}

import { useTranslation } from 'react-i18next';

import { Button } from '../../components/ui/Button';
import { Dialog } from '../../components/ui/Dialog';
import { useToast } from '../../components/ui/toast-context';
import { useSettings, useUpdateSettings } from '../settings/useSettings';

/**
 * One-time KVKK notice: shown until `consentNoticeAcknowledged` is true. It cannot be dismissed
 * any other way than acknowledging it. If the settings cannot be read the acknowledgement is
 * unknown, so the notice is shown anyway (with the load error) rather than silently skipped.
 */
export function ConsentNotice() {
  const { t } = useTranslation();
  const toast = useToast();
  const { data: settings, isError, refetch } = useSettings();
  const update = useUpdateSettings();

  const open = settings === undefined ? isError : !settings.consentNoticeAcknowledged;
  const loadFailed = settings === undefined && isError;

  function acknowledge() {
    update.mutate({ consentNoticeAcknowledged: true }, { onError: (error) => toast.error(error) });
  }

  return (
    <Dialog
      open={open}
      onClose={() => undefined}
      dismissible={false}
      title={t('consent.title')}
      description={t('consent.body')}
      footer={
        <>
          {loadFailed ? <Button onClick={() => void refetch()}>{t('common.retry')}</Button> : null}
          <Button
            variant="primary"
            data-autofocus
            onClick={acknowledge}
            disabled={update.isPending}
          >
            {t('consent.acknowledge')}
          </Button>
        </>
      }
    >
      {loadFailed ? (
        <p className="field__error" role="alert">
          {t('errors.settingsLoad')}
        </p>
      ) : null}
    </Dialog>
  );
}

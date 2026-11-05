import { useState } from 'react';
import { useTranslation } from 'react-i18next';

import { Button } from '../../components/ui/Button';
import { useVaultStatus } from './api';
import { SecretsTable } from './SecretsTable';
import { UnlockDialog, type VaultDialogMode } from './UnlockDialog';
import { VariablesTable } from './VariablesTable';

import '../../layout/layout.css';
import './credentials.css';

export default function CredentialsPage() {
  const { t } = useTranslation();
  const status = useVaultStatus();
  // The dialog opens by itself when the vault needs a password; "cancel" keeps it closed until the
  // user asks again with the button in the vault panel.
  const [dialogDismissed, setDialogDismissed] = useState(false);

  const vault = status.data;
  const needsPassword =
    vault !== undefined &&
    vault.available &&
    (vault.locked || (vault.backend === 'encrypted_file' && !vault.masterPasswordSet));
  const mode: VaultDialogMode =
    vault?.backend === 'encrypted_file' && !vault.masterPasswordSet ? 'setup' : 'unlock';

  return (
    <section className="page">
      <header className="page__header">
        <h1>{t('credentials.title')}</h1>
        <p className="page__intro">{t('credentials.intro')}</p>
      </header>

      <VariablesTable />

      {status.isPending ? (
        <p role="status">{t('common.loading')}</p>
      ) : status.isError ? (
        <div className="alert" role="alert">
          <span>{t('credentials.vault.statusFailed')}</span>
          <Button size="sm" onClick={() => void status.refetch()}>
            {t('common.retry')}
          </Button>
        </div>
      ) : !status.data.available ? (
        <div className="credentials-vault" role="alert">
          <h2 className="credentials-vault__title">{t('credentials.vault.unavailableTitle')}</h2>
          <p className="credentials-vault__body">{t('credentials.vault.unavailableBody')}</p>
        </div>
      ) : needsPassword ? (
        <div className="credentials-vault">
          <h2 className="credentials-vault__title">
            {mode === 'setup'
              ? t('credentials.vault.setupTitle')
              : t('credentials.vault.lockedTitle')}
          </h2>
          <p className="credentials-vault__body">
            {mode === 'setup'
              ? t('credentials.vault.setupBody')
              : t('credentials.vault.lockedBody')}
          </p>
          <Button variant="primary" onClick={() => setDialogDismissed(false)}>
            {mode === 'setup' ? t('credentials.vault.setup') : t('credentials.vault.unlock')}
          </Button>
        </div>
      ) : (
        <SecretsTable />
      )}

      <UnlockDialog
        open={needsPassword && !dialogDismissed}
        mode={mode}
        onClose={() => setDialogDismissed(true)}
      />
    </section>
  );
}

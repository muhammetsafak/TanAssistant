import { useState, type FormEvent } from 'react';
import { useTranslation } from 'react-i18next';

import { Button } from '../../components/ui/Button';
import { Dialog } from '../../components/ui/Dialog';
import { WarningIcon } from '../../components/ui/Icons';
import { TextField } from '../../components/ui/TextField';
import { describeError } from '../../ipc/client';
import { useSetMasterPassword, useUnlockVault } from './api';

import '../../components/ui/ui.css';

export type VaultDialogMode = 'unlock' | 'setup';

/** Master-password dialog for the encrypted-file vault: unlock it, or set the first password. */
export function UnlockDialog({
  open,
  mode,
  onClose,
}: {
  open: boolean;
  mode: VaultDialogMode;
  onClose: () => void;
}) {
  if (!open) return null;
  return <PasswordForm mode={mode} onClose={onClose} />;
}

function PasswordForm({ mode, onClose }: { mode: VaultDialogMode; onClose: () => void }) {
  const { t } = useTranslation();
  const unlock = useUnlockVault();
  const setMasterPassword = useSetMasterPassword();
  const [password, setPassword] = useState('');
  const [confirm, setConfirm] = useState('');
  const [failure, setFailure] = useState<string | null>(null);
  const prefix = mode === 'setup' ? 'credentials.setup' : 'credentials.unlock';
  const pending = unlock.isPending || setMasterPassword.isPending;

  function submit(event: FormEvent) {
    event.preventDefault();
    if (!password) {
      setFailure(t(`${prefix}.passwordRequired`));
      return;
    }
    if (mode === 'setup' && password !== confirm) {
      setFailure(t('credentials.setup.mismatch'));
      return;
    }
    setFailure(null);

    const callbacks = {
      onSuccess: onClose,
      // Failures stay inline in the dialog (e.g. a wrong password) so the user can retry at once.
      onError: (error: unknown) => setFailure(describeError(error, t).message),
    };
    if (mode === 'setup') {
      setMasterPassword.mutate(password, callbacks);
    } else {
      unlock.mutate(password, callbacks);
    }
  }

  return (
    // While the request is in flight the dialog stays open, so a wrong-password error is shown.
    <Dialog
      open
      onClose={onClose}
      dismissible={!pending}
      title={t(`${prefix}.title`)}
      description={t(`${prefix}.description`)}
    >
      <form className="dialog__form" onSubmit={submit} noValidate>
        <TextField
          type="password"
          label={t(`${prefix}.password`)}
          value={password}
          onChange={(event) => setPassword(event.target.value)}
          autoComplete={mode === 'setup' ? 'new-password' : 'current-password'}
        />
        {mode === 'setup' ? (
          <TextField
            type="password"
            label={t('credentials.setup.confirm')}
            value={confirm}
            onChange={(event) => setConfirm(event.target.value)}
            autoComplete="new-password"
          />
        ) : null}
        {failure ? (
          <p className="field__error" role="alert">
            <WarningIcon className="field__error-icon" />
            {failure}
          </p>
        ) : null}
        <div className="dialog__footer">
          <Button onClick={onClose} disabled={pending}>
            {t('common.cancel')}
          </Button>
          <Button type="submit" variant="primary" disabled={pending}>
            {t(`${prefix}.submit`)}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

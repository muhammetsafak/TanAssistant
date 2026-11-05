import { useState, type FormEvent } from 'react';
import { useTranslation } from 'react-i18next';

import { Button } from '../../components/ui/Button';
import { Dialog } from '../../components/ui/Dialog';
import { TextField } from '../../components/ui/TextField';
import { useToast } from '../../components/ui/toast-context';
import type { SecretDto } from '../../ipc/client';
import { useCreateSecret, useCredentialsErrorHandler, useUpdateSecret } from './api';

import '../../components/ui/ui.css';

export type SecretTarget = SecretDto | 'new';

/**
 * Create (`'new'`) or replace (an existing secret) dialog; closed when `target` is null.
 * `onSaved` fires after a successful write, with the secret's id when it was a replacement.
 */
export function SecretDialog({
  target,
  onClose,
  onSaved,
}: {
  target: SecretTarget | null;
  onClose: () => void;
  onSaved?: (secretId: string | null) => void;
}) {
  if (target === null) return null;
  return (
    <SecretForm secret={target === 'new' ? null : target} onClose={onClose} onSaved={onSaved} />
  );
}

function SecretForm({
  secret,
  onClose,
  onSaved,
}: {
  secret: SecretDto | null;
  onClose: () => void;
  onSaved?: (secretId: string | null) => void;
}) {
  const { t } = useTranslation();
  const toast = useToast();
  const handleError = useCredentialsErrorHandler();
  const create = useCreateSecret();
  const update = useUpdateSecret();
  const [name, setName] = useState(secret?.name ?? '');
  const [description, setDescription] = useState(secret?.description ?? '');
  // The existing value is never loaded into the form; an empty field means "keep it".
  const [value, setValue] = useState('');
  const [errors, setErrors] = useState<{ name?: string; value?: string }>({});
  const pending = create.isPending || update.isPending;

  function submit(event: FormEvent) {
    event.preventDefault();
    const next = {
      name: name.trim() ? undefined : t('credentials.secrets.nameRequired'),
      value: secret || value ? undefined : t('credentials.secrets.valueRequired'),
    };
    setErrors(next);
    if (next.name || next.value) return;

    const callbacks = {
      onSuccess: () => {
        toast.success(t('credentials.secrets.saved'));
        onSaved?.(secret?.id ?? null);
        onClose();
      },
      onError: handleError,
    };

    if (secret) {
      update.mutate(
        {
          id: secret.id,
          name: name.trim(),
          description: description.trim(),
          value: value ? value : null,
        },
        callbacks,
      );
    } else {
      create.mutate({ name: name.trim(), description: description.trim(), value }, callbacks);
    }
  }

  return (
    // While a write is in flight the dialog stays open, so its result (success or error) is
    // always handled by this still-mounted form.
    <Dialog
      open
      onClose={onClose}
      dismissible={!pending}
      title={secret ? t('credentials.secrets.editTitle') : t('credentials.secrets.addTitle')}
    >
      <form className="dialog__form" onSubmit={submit} noValidate>
        <TextField
          label={t('credentials.secrets.name')}
          value={name}
          onChange={(event) => setName(event.target.value)}
          error={errors.name}
          autoComplete="off"
          spellCheck={false}
        />
        <TextField
          label={t('credentials.secrets.description')}
          value={description}
          onChange={(event) => setDescription(event.target.value)}
          autoComplete="off"
        />
        <TextField
          type="password"
          label={
            secret ? t('credentials.secrets.valueLabelEdit') : t('credentials.secrets.valueLabel')
          }
          hint={secret ? t('credentials.secrets.valueHintEdit') : undefined}
          value={value}
          onChange={(event) => setValue(event.target.value)}
          error={errors.value}
          autoComplete="new-password"
          spellCheck={false}
        />
        <div className="dialog__footer">
          <Button onClick={onClose} disabled={pending}>
            {t('common.cancel')}
          </Button>
          <Button type="submit" variant="primary" disabled={pending}>
            {t('common.save')}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

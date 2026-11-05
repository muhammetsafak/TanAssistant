import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { Button } from '../../components/ui/Button';
import { ConfirmDialog } from '../../components/ui/ConfirmDialog';
import { useToast } from '../../components/ui/toast-context';
import type { SecretDto } from '../../ipc/client';
import { refocusIfLost } from './focus';
import {
  REVEAL_DURATION_MS,
  revealSecret,
  useCredentialsErrorHandler,
  useDeleteSecret,
  useSecrets,
} from './api';
import { SecretDialog, type SecretTarget } from './SecretDialog';

import '../../components/ui/ui.css';
import './credentials.css';

export function SecretsTable() {
  const { t } = useTranslation();
  const toast = useToast();
  const handleError = useCredentialsErrorHandler();
  const secrets = useSecrets(true);
  const deleteSecret = useDeleteSecret();
  const [editing, setEditing] = useState<SecretTarget | null>(null);
  const [deleting, setDeleting] = useState<SecretDto | null>(null);
  // Bumped when a secret's value is replaced, which remounts its row and so drops any revealed
  // (now stale) value.
  const [revisions, setRevisions] = useState<Record<string, number>>({});
  const addButtonRef = useRef<HTMLButtonElement>(null);

  const rows = [...(secrets.data ?? [])].sort((a, b) => a.name.localeCompare(b.name));

  function confirmDelete() {
    if (!deleting) return;
    deleteSecret.mutate(deleting.id, {
      onSuccess: () => {
        toast.success(t('credentials.secrets.deleted'));
        setDeleting(null);
        refocusIfLost(addButtonRef.current);
      },
      onError: (error) => {
        handleError(error);
        setDeleting(null);
      },
    });
  }

  return (
    <section className="credentials-section" aria-labelledby="secrets-heading">
      <div className="credentials-section__header">
        <div>
          <h2 className="credentials-section__title" id="secrets-heading">
            {t('credentials.secrets.title')}
          </h2>
          <p className="credentials-section__hint">{t('credentials.secrets.hint')}</p>
        </div>
        <Button ref={addButtonRef} variant="primary" onClick={() => setEditing('new')}>
          {t('credentials.secrets.add')}
        </Button>
      </div>

      {secrets.isPending ? (
        <p role="status">{t('common.loading')}</p>
      ) : secrets.isError ? (
        <div className="alert" role="alert">
          <span>{t('credentials.secrets.loadFailed')}</span>
          <Button size="sm" onClick={() => void secrets.refetch()}>
            {t('common.retry')}
          </Button>
        </div>
      ) : rows.length === 0 ? (
        <p className="credentials-empty">{t('credentials.secrets.empty')}</p>
      ) : (
        <>
          <div className="table-scroll">
            <table className="table">
              <thead>
                <tr>
                  <th scope="col">{t('credentials.secrets.name')}</th>
                  <th scope="col">{t('credentials.secrets.description')}</th>
                  <th scope="col">{t('credentials.secrets.createdAt')}</th>
                  <th scope="col">{t('credentials.secrets.value')}</th>
                  <th scope="col">{t('credentials.secrets.actions')}</th>
                </tr>
              </thead>
              <tbody>
                {rows.map((secret) => (
                  <SecretRow
                    key={`${secret.id}:${revisions[secret.id] ?? 0}`}
                    secret={secret}
                    onEdit={() => setEditing(secret)}
                    onDelete={() => setDeleting(secret)}
                  />
                ))}
              </tbody>
            </table>
          </div>
          <p className="secret-note">{t('credentials.secrets.revealNote')}</p>
        </>
      )}

      <SecretDialog
        target={editing}
        onClose={() => setEditing(null)}
        onSaved={(secretId) => {
          if (secretId === null) return;
          setRevisions((current) => ({ ...current, [secretId]: (current[secretId] ?? 0) + 1 }));
        }}
      />
      <ConfirmDialog
        open={deleting !== null}
        title={t('credentials.secrets.deleteTitle')}
        message={t('credentials.secrets.deleteMessage', { name: deleting?.name ?? '' })}
        confirmLabel={t('common.delete')}
        pending={deleteSecret.isPending}
        onConfirm={confirmDelete}
        onCancel={() => setDeleting(null)}
      />
    </section>
  );
}

function SecretRow({
  secret,
  onEdit,
  onDelete,
}: {
  secret: SecretDto;
  onEdit: () => void;
  onDelete: () => void;
}) {
  const { t, i18n } = useTranslation();
  const toast = useToast();
  const handleError = useCredentialsErrorHandler();
  // The value lives only in this component's state, and only while it is being shown.
  const [revealed, setRevealed] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (revealed === null) return;
    const timer = setTimeout(() => setRevealed(null), REVEAL_DURATION_MS);
    return () => clearTimeout(timer);
  }, [revealed]);

  async function toggleReveal() {
    if (revealed !== null) {
      setRevealed(null);
      return;
    }
    setBusy(true);
    try {
      setRevealed(await revealSecret(secret.id));
    } catch (error) {
      handleError(error);
    } finally {
      setBusy(false);
    }
  }

  async function copy() {
    setBusy(true);
    try {
      // Always ask the vault again: never copy a value that was revealed earlier.
      const value = await revealSecret(secret.id);
      try {
        await navigator.clipboard.writeText(value);
      } catch {
        toast.errorText(t('errors.clipboard'));
        return;
      }
      toast.success(t('credentials.secrets.copied'));
    } catch (error) {
      handleError(error);
    } finally {
      setBusy(false);
    }
  }

  const created =
    secret.createdAt === null
      ? t('common.notAvailable')
      : new Intl.DateTimeFormat(i18n.language, { dateStyle: 'medium', timeStyle: 'short' }).format(
          new Date(secret.createdAt),
        );

  return (
    <tr>
      <td className="cell-key">{secret.name}</td>
      <td className="cell-description">{secret.description || t('common.notAvailable')}</td>
      <td>{created}</td>
      <td>
        {revealed === null ? (
          <span className="secret-masked">
            <span aria-hidden="true">••••••</span>
            <span className="sr-only">{t('credentials.secrets.masked')}</span>
          </span>
        ) : (
          <code className="secret-value">{revealed}</code>
        )}
      </td>
      <td>
        <div className="cell-actions">
          <Button
            size="sm"
            onClick={() => void toggleReveal()}
            disabled={busy}
            aria-label={
              revealed === null
                ? t('credentials.secrets.reveal', { name: secret.name })
                : t('credentials.secrets.hide', { name: secret.name })
            }
          >
            {revealed === null
              ? t('credentials.secrets.revealText')
              : t('credentials.secrets.hideText')}
          </Button>
          <Button
            size="sm"
            onClick={() => void copy()}
            disabled={busy}
            aria-label={t('credentials.secrets.copy', { name: secret.name })}
          >
            {t('credentials.secrets.copyText')}
          </Button>
          <Button
            size="sm"
            onClick={onEdit}
            aria-label={t('credentials.secrets.edit', { name: secret.name })}
          >
            {t('credentials.secrets.editText')}
          </Button>
          <Button
            size="sm"
            onClick={onDelete}
            aria-label={t('credentials.secrets.delete', { name: secret.name })}
          >
            {t('common.delete')}
          </Button>
        </div>
      </td>
    </tr>
  );
}

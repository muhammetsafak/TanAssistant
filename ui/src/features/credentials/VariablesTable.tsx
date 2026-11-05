import { useRef, useState, type FormEvent } from 'react';
import { useTranslation } from 'react-i18next';

import { Button } from '../../components/ui/Button';
import { ConfirmDialog } from '../../components/ui/ConfirmDialog';
import { Dialog } from '../../components/ui/Dialog';
import { TextField } from '../../components/ui/TextField';
import { useToast } from '../../components/ui/toast-context';
import type { VariableDto } from '../../ipc/client';
import { refocusIfLost } from './focus';
import {
  useCredentialsErrorHandler,
  useDeleteVariable,
  useUpsertVariable,
  useVariables,
} from './api';

import '../../components/ui/ui.css';
import './credentials.css';

type VariableTarget = VariableDto | 'new';

export function VariablesTable() {
  const { t } = useTranslation();
  const toast = useToast();
  const handleError = useCredentialsErrorHandler();
  const variables = useVariables();
  const deleteVariable = useDeleteVariable();
  const [editing, setEditing] = useState<VariableTarget | null>(null);
  const [deleting, setDeleting] = useState<VariableDto | null>(null);
  const addButtonRef = useRef<HTMLButtonElement>(null);

  const rows = [...(variables.data ?? [])].sort((a, b) => a.key.localeCompare(b.key));

  function confirmDelete() {
    if (!deleting) return;
    deleteVariable.mutate(deleting.id, {
      onSuccess: () => {
        toast.success(t('credentials.variables.deleted'));
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
    <section className="credentials-section" aria-labelledby="variables-heading">
      <div className="credentials-section__header">
        <div>
          <h2 className="credentials-section__title" id="variables-heading">
            {t('credentials.variables.title')}
          </h2>
          <p className="credentials-section__hint">{t('credentials.variables.hint')}</p>
        </div>
        <Button ref={addButtonRef} variant="primary" onClick={() => setEditing('new')}>
          {t('credentials.variables.add')}
        </Button>
      </div>

      {variables.isPending ? (
        <p role="status">{t('common.loading')}</p>
      ) : variables.isError ? (
        <div className="alert" role="alert">
          <span>{t('credentials.variables.loadFailed')}</span>
          <Button size="sm" onClick={() => void variables.refetch()}>
            {t('common.retry')}
          </Button>
        </div>
      ) : rows.length === 0 ? (
        <p className="credentials-empty">{t('credentials.variables.empty')}</p>
      ) : (
        <div className="table-scroll">
          <table className="table">
            <thead>
              <tr>
                <th scope="col">{t('credentials.variables.key')}</th>
                <th scope="col">{t('credentials.variables.value')}</th>
                <th scope="col">{t('credentials.variables.description')}</th>
                <th scope="col">{t('credentials.variables.actions')}</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((variable) => (
                <tr key={variable.id}>
                  <td className="cell-key">{variable.key}</td>
                  <td>
                    <code className="cell-value" title={variable.value}>
                      {variable.value}
                    </code>
                  </td>
                  <td className="cell-description">
                    {variable.description || t('common.notAvailable')}
                  </td>
                  <td>
                    <div className="cell-actions">
                      <Button
                        size="sm"
                        onClick={() => setEditing(variable)}
                        aria-label={t('credentials.variables.edit', { name: variable.key })}
                      >
                        {t('common.edit')}
                      </Button>
                      <Button
                        size="sm"
                        onClick={() => setDeleting(variable)}
                        aria-label={t('credentials.variables.delete', { name: variable.key })}
                      >
                        {t('common.delete')}
                      </Button>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      <VariableDialog target={editing} onClose={() => setEditing(null)} />
      <ConfirmDialog
        open={deleting !== null}
        title={t('credentials.variables.deleteTitle')}
        message={t('credentials.variables.deleteMessage', { name: deleting?.key ?? '' })}
        confirmLabel={t('common.delete')}
        pending={deleteVariable.isPending}
        onConfirm={confirmDelete}
        onCancel={() => setDeleting(null)}
      />
    </section>
  );
}

function VariableDialog({
  target,
  onClose,
}: {
  target: VariableTarget | null;
  onClose: () => void;
}) {
  if (target === null) return null;
  return <VariableForm variable={target === 'new' ? null : target} onClose={onClose} />;
}

function VariableForm({
  variable,
  onClose,
}: {
  variable: VariableDto | null;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const toast = useToast();
  const handleError = useCredentialsErrorHandler();
  const upsert = useUpsertVariable();
  const [key, setKey] = useState(variable?.key ?? '');
  const [value, setValue] = useState(variable?.value ?? '');
  const [description, setDescription] = useState(variable?.description ?? '');
  const [errors, setErrors] = useState<{ key?: string; value?: string }>({});

  function submit(event: FormEvent) {
    event.preventDefault();
    const next = {
      key: key.trim() ? undefined : t('credentials.variables.keyRequired'),
      value: value ? undefined : t('credentials.variables.valueRequired'),
    };
    setErrors(next);
    if (next.key || next.value) return;

    upsert.mutate(
      { id: variable?.id ?? null, key: key.trim(), value, description: description.trim() },
      {
        onSuccess: () => {
          toast.success(t('credentials.variables.saved'));
          onClose();
        },
        onError: handleError,
      },
    );
  }

  return (
    // While a write is in flight the dialog stays open, so its result is always handled here.
    <Dialog
      open
      onClose={onClose}
      dismissible={!upsert.isPending}
      title={variable ? t('credentials.variables.editTitle') : t('credentials.variables.addTitle')}
    >
      <form className="dialog__form" onSubmit={submit} noValidate>
        <TextField
          label={t('credentials.variables.key')}
          value={key}
          onChange={(event) => setKey(event.target.value)}
          error={errors.key}
          autoComplete="off"
          spellCheck={false}
        />
        <TextField
          label={t('credentials.variables.value')}
          value={value}
          onChange={(event) => setValue(event.target.value)}
          error={errors.value}
          autoComplete="off"
          spellCheck={false}
        />
        <TextField
          label={t('credentials.variables.description')}
          value={description}
          onChange={(event) => setDescription(event.target.value)}
          autoComplete="off"
        />
        <div className="dialog__footer">
          <Button onClick={onClose} disabled={upsert.isPending}>
            {t('common.cancel')}
          </Button>
          <Button type="submit" variant="primary" disabled={upsert.isPending}>
            {t('common.save')}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

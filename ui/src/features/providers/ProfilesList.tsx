import { useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { Button } from '../../components/ui/Button';
import { ConfirmDialog } from '../../components/ui/ConfirmDialog';
import { useToast } from '../../components/ui/toast-context';
import { refocusIfLost } from '../credentials/focus';
import {
  KIND_LABELS,
  useAiErrorHandler,
  useDeleteProfile,
  useProfiles,
  type ProviderProfile,
} from './api';
import { DeprecationBadge } from './ModelDeprecation';
import { ProfileForm, type ProfileTarget } from './ProfileForm';
import { TestConnectionButton } from './TestConnectionButton';

import '../../components/ui/ui.css';
import './providers.css';

const CAPABILITIES = ['chat', 'vision', 'transcription'] as const;

export function ProfilesList() {
  const { t } = useTranslation();
  const toast = useToast();
  const handleError = useAiErrorHandler();
  const profiles = useProfiles();
  const deleteProfile = useDeleteProfile();
  const [editing, setEditing] = useState<ProfileTarget | null>(null);
  const [deleting, setDeleting] = useState<ProviderProfile | null>(null);
  const addButtonRef = useRef<HTMLButtonElement>(null);

  const rows = [...(profiles.data ?? [])].sort((a, b) =>
    a.displayName.localeCompare(b.displayName),
  );

  function confirmDelete() {
    if (!deleting) return;
    deleteProfile.mutate(deleting.id, {
      onSuccess: () => {
        toast.success(t('providers.list.deleted'));
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
    <section className="providers-section" aria-labelledby="profiles-heading">
      <div className="providers-section__header">
        <div>
          <h2 className="providers-section__title" id="profiles-heading">
            {t('providers.list.title')}
          </h2>
          <p className="providers-section__hint">{t('providers.list.hint')}</p>
        </div>
        <Button ref={addButtonRef} variant="primary" onClick={() => setEditing('new')}>
          {t('providers.list.add')}
        </Button>
      </div>

      {profiles.isPending ? (
        <p role="status">{t('common.loading')}</p>
      ) : profiles.isError ? (
        <div className="alert" role="alert">
          <span>{t('providers.list.loadFailed')}</span>
          <Button size="sm" onClick={() => void profiles.refetch()}>
            {t('common.retry')}
          </Button>
        </div>
      ) : rows.length === 0 ? (
        <p className="providers-empty">{t('providers.list.empty')}</p>
      ) : (
        <div className="table-scroll">
          <table className="table">
            <thead>
              <tr>
                <th scope="col">{t('providers.list.name')}</th>
                <th scope="col">{t('providers.list.provider')}</th>
                <th scope="col">{t('providers.list.model')}</th>
                <th scope="col">{t('providers.list.capabilities')}</th>
                <th scope="col">{t('providers.list.actions')}</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((profile) => (
                <tr key={profile.id}>
                  <th scope="row" className="cell-name">
                    {profile.displayName}
                  </th>
                  <td>{KIND_LABELS[profile.kind]}</td>
                  <td className="cell-model">
                    {profile.model}
                    <DeprecationBadge kind={profile.kind} model={profile.model} />
                  </td>
                  <td>
                    <ul className="badge-list">
                      {CAPABILITIES.filter((capability) => profile.capabilities[capability]).map(
                        (capability) => (
                          <li key={capability} className="badge">
                            {t(`providers.capabilities.${capability}`)}
                          </li>
                        ),
                      )}
                    </ul>
                  </td>
                  <td className="cell-test">
                    <div className="provider-actions">
                      <TestConnectionButton
                        key={[
                          profile.id,
                          profile.kind,
                          profile.model,
                          profile.baseUrl,
                          profile.secretRef,
                        ].join('|')}
                        profile={profile}
                      />
                      <Button
                        size="sm"
                        onClick={() => setEditing(profile)}
                        aria-label={t('providers.list.edit', { name: profile.displayName })}
                      >
                        {t('common.edit')}
                      </Button>
                      <Button
                        size="sm"
                        onClick={() => setDeleting(profile)}
                        aria-label={t('providers.list.delete', { name: profile.displayName })}
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

      <ProfileForm target={editing} onClose={() => setEditing(null)} />
      <ConfirmDialog
        open={deleting !== null}
        title={t('providers.list.deleteTitle')}
        message={t('providers.list.deleteMessage', { name: deleting?.displayName ?? '' })}
        confirmLabel={t('common.delete')}
        pending={deleteProfile.isPending}
        onConfirm={confirmDelete}
        onCancel={() => setDeleting(null)}
      />
    </section>
  );
}

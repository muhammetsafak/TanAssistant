import { useTranslation } from 'react-i18next';

import { Button } from '../../components/ui/Button';
import { useToast } from '../../components/ui/toast-context';
import {
  KIND_LABELS,
  TASKS,
  capabilityFor,
  useAiErrorHandler,
  useAssignments,
  useProfiles,
  useSetAssignment,
  type ProviderProfile,
  type TaskKind,
} from './api';
import { SelectField } from './SelectField';

import '../../components/ui/ui.css';
import './providers.css';

/** Why `profile` cannot do `task`, or `null` when it can. */
function unsupportedReason(
  profile: ProviderProfile,
  task: TaskKind,
  t: ReturnType<typeof useTranslation>['t'],
): string | null {
  if (profile.capabilities[capabilityFor(task)]) return null;
  if (task === 'vision' && profile.kind === 'ollama') return t('providers.assign.ollamaNoVision');
  return t(`providers.assign.unsupported.${task}`, { provider: KIND_LABELS[profile.kind] });
}

export function TaskAssignments() {
  const { t } = useTranslation();
  const profiles = useProfiles();
  const assignments = useAssignments();

  return (
    <section className="providers-section" aria-labelledby="assignments-heading">
      <div>
        <h2 className="providers-section__title" id="assignments-heading">
          {t('providers.assign.title')}
        </h2>
        <p className="providers-section__hint">{t('providers.assign.hint')}</p>
      </div>

      {profiles.isPending || assignments.isPending ? (
        <p role="status">{t('common.loading')}</p>
      ) : profiles.isError || assignments.isError ? (
        <div className="alert" role="alert">
          <span>{t('providers.assign.loadFailed')}</span>
          <Button
            size="sm"
            onClick={() => {
              if (profiles.isError) void profiles.refetch();
              if (assignments.isError) void assignments.refetch();
            }}
          >
            {t('common.retry')}
          </Button>
        </div>
      ) : (
        <div className="assign-grid">
          {TASKS.map((task) => (
            <AssignmentRow
              key={task}
              task={task}
              profiles={profiles.data}
              assignedId={assignments.data.find((entry) => entry.task === task)?.profileId ?? null}
            />
          ))}
        </div>
      )}
    </section>
  );
}

function AssignmentRow({
  task,
  profiles,
  assignedId,
}: {
  task: TaskKind;
  profiles: ProviderProfile[];
  assignedId: string | null;
}) {
  const { t } = useTranslation();
  const toast = useToast();
  const handleError = useAiErrorHandler();
  const setAssignment = useSetAssignment();

  const sorted = [...profiles].sort((a, b) => a.displayName.localeCompare(b.displayName));
  // While a change is in flight the select shows the choice being saved, not the old one.
  const requested = setAssignment.isPending ? setAssignment.variables.profileId : assignedId;
  const selected = requested !== null && profiles.some((p) => p.id === requested) ? requested : '';
  const selectedProfile = sorted.find((p) => p.id === selected);
  const staleReason = selectedProfile ? unsupportedReason(selectedProfile, task, t) : null;
  const isTranscription = task === 'transcription';

  function change(profileId: string) {
    setAssignment.mutate(
      { task, profileId: profileId === '' ? null : profileId },
      {
        onSuccess: () =>
          toast.success(t('providers.assign.saved', { task: t(`providers.tasks.${task}`) })),
        onError: handleError,
      },
    );
  }

  return (
    <SelectField
      label={t(`providers.tasks.${task}`)}
      value={selected}
      onChange={(event) => change(event.target.value)}
      aria-busy={setAssignment.isPending}
      hint={t(`providers.assign.taskHint.${task}`)}
      error={staleReason ? t('providers.assign.stale', { reason: staleReason }) : null}
    >
      <option value="">
        {isTranscription ? t('providers.assign.localWhisper') : t('providers.assign.none')}
      </option>
      {sorted.map((profile) => {
        const reason = unsupportedReason(profile, task, t);
        const label = `${profile.displayName} (${KIND_LABELS[profile.kind]}, ${profile.model})`;
        return (
          <option
            key={profile.id}
            value={profile.id}
            disabled={reason !== null}
            title={reason ?? undefined}
          >
            {reason ? `${label} — ${reason}` : label}
          </option>
        );
      })}
    </SelectField>
  );
}

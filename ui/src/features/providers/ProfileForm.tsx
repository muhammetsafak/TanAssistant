import { useEffect, useRef, useState, type FormEvent } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { Trans, useTranslation } from 'react-i18next';
import { Link } from 'react-router';

import { Button } from '../../components/ui/Button';
import { Dialog } from '../../components/ui/Dialog';
import { WarningIcon } from '../../components/ui/Icons';
import { TextField } from '../../components/ui/TextField';
import { useToast } from '../../components/ui/toast-context';
import { ipc } from '../../ipc/client';
import { credentialKeys, useSecrets, useVaultStatus } from '../credentials/api';
import { SecretDialog } from '../credentials/SecretDialog';
import {
  PROVIDER_KINDS,
  kindLabel,
  useAiErrorHandler,
  useProviderKinds,
  useUpsertProfile,
  type ProviderKind,
  type ProviderKindInfo,
  type ProviderProfile,
} from './api';
import { DeprecationNote } from './ModelDeprecation';
import { SelectField } from './SelectField';

import '../../components/ui/ui.css';
import './providers.css';

export type ProfileTarget = ProviderProfile | 'new';

/** Create (`'new'`) or edit dialog for a provider profile; closed when `target` is null. */
export function ProfileForm({
  target,
  onClose,
}: {
  target: ProfileTarget | null;
  onClose: () => void;
}) {
  if (target === null) return null;
  return <ProfileFormLoader profile={target === 'new' ? null : target} onClose={onClose} />;
}

function ProfileFormLoader({
  profile,
  onClose,
}: {
  profile: ProviderProfile | null;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const kinds = useProviderKinds();
  const title = profile ? t('providers.form.editTitle') : t('providers.form.addTitle');

  if (kinds.data) {
    return <FormBody profile={profile} kinds={kinds.data} onClose={onClose} />;
  }
  return (
    <Dialog open onClose={onClose} title={title}>
      {kinds.isError ? (
        <div className="alert" role="alert">
          <span>{t('providers.form.kindsFailed')}</span>
          <Button size="sm" onClick={() => void kinds.refetch()}>
            {t('common.retry')}
          </Button>
        </div>
      ) : (
        <p role="status">{t('common.loading')}</p>
      )}
      <div className="dialog__footer">
        <Button onClick={onClose}>{t('common.cancel')}</Button>
      </div>
    </Dialog>
  );
}

/** Ollama's endpoint is a local server, so its default is filled in; other kinds leave it empty. */
function prefilledBaseUrl(info: ProviderKindInfo | undefined): string {
  return info?.kind === 'ollama' ? info.defaultBaseUrl : '';
}

function isHttpUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol === 'http:' || url.protocol === 'https:';
  } catch {
    return false;
  }
}

type SuggestionTask = 'chat' | 'vision' | 'transcription';

/** Distinct suggested models, each with the tasks it is suggested for. */
function modelSuggestions(info: ProviderKindInfo | undefined) {
  const byModel = new Map<string, SuggestionTask[]>();
  if (!info) return [];
  for (const task of ['chat', 'vision', 'transcription'] as const) {
    const model = info.suggestedModels[task];
    if (model) byModel.set(model, [...(byModel.get(model) ?? []), task]);
  }
  return Array.from(byModel, ([model, tasks]) => ({ model, tasks }));
}

interface FormErrors {
  displayName?: string;
  model?: string;
  baseUrl?: string;
  secret?: string;
}

function FormBody({
  profile,
  kinds,
  onClose,
}: {
  profile: ProviderProfile | null;
  kinds: ProviderKindInfo[];
  onClose: () => void;
}) {
  const { t, i18n } = useTranslation();
  const toast = useToast();
  const queryClient = useQueryClient();
  const handleError = useAiErrorHandler();
  const upsert = useUpsertProfile();
  const vault = useVaultStatus();
  const formRef = useRef<HTMLFormElement>(null);

  const infoOf = (kind: ProviderKind) => kinds.find((entry) => entry.kind === kind);
  const [kind, setKind] = useState<ProviderKind>(profile?.kind ?? 'openai');
  const [displayName, setDisplayName] = useState(profile?.displayName ?? '');
  const [model, setModel] = useState(profile?.model ?? '');
  const [baseUrl, setBaseUrl] = useState(
    profile ? (profile.baseUrl ?? prefilledBaseUrl(infoOf(profile.kind))) : '',
  );
  const [secretRef, setSecretRef] = useState(profile?.secretRef ?? '');
  const [supportsVision, setSupportsVision] = useState(profile?.supportsVision ?? false);
  const [errors, setErrors] = useState<FormErrors>({});
  const [attempt, setAttempt] = useState(0);
  const [newSecretOpen, setNewSecretOpen] = useState(false);
  // The ids the list held when "new secret" was clicked; null when the list was not loaded then.
  const knownSecretIds = useRef<Set<string> | null>(null);

  const info = infoOf(kind);
  const requiresCli = info?.requiresCli ?? false;
  // A CLI kind without its own wording falls back to the generic note instead of a raw key.
  const cliNoteKey = i18n.exists(`providers.form.cliNote.${kind}`)
    ? `providers.form.cliNote.${kind}`
    : 'providers.form.cliNoteGeneric';
  const requiresApiKey = !requiresCli && (info?.requiresApiKey ?? true);
  const isOllama = kind === 'ollama';

  const vaultStatus = vault.data;
  const needsUnlock =
    vaultStatus !== undefined &&
    vaultStatus.available &&
    (vaultStatus.locked ||
      (vaultStatus.backend === 'encrypted_file' && !vaultStatus.masterPasswordSet));
  const vaultUnavailable = vaultStatus !== undefined && !vaultStatus.available;
  const secrets = useSecrets(vaultStatus?.available === true);
  const secretList = [...(secrets.data ?? [])].sort((a, b) => a.name.localeCompare(b.name));
  const secretMissing =
    secretRef !== '' && secrets.data !== undefined && !secrets.data.some((s) => s.id === secretRef);
  const secretsReady = secrets.data !== undefined;

  // Move focus to the first invalid field after a failed submit.
  useEffect(() => {
    if (attempt === 0) return;
    formRef.current?.querySelector<HTMLElement>('[aria-invalid="true"]')?.focus();
  }, [attempt]);

  function changeKind(next: ProviderKind) {
    // An endpoint the user has not touched follows the provider; a typed one is kept.
    if (baseUrl === prefilledBaseUrl(info)) setBaseUrl(prefilledBaseUrl(infoOf(next)));
    if (next !== 'ollama') setSupportsVision(false);
    if (infoOf(next)?.requiresCli) {
      // A CLI profile has neither an endpoint nor a key: whatever was typed or picked is dropped.
      setBaseUrl('');
      setSecretRef('');
      setErrors((current) => ({ ...current, baseUrl: undefined, secret: undefined }));
    }
    setKind(next);
  }

  async function handleSecretCreated() {
    // The new secret's id is not reported by the dialog: it is the one the list did not have.
    // Without a known "before" list, or with several new entries, guessing could pick the wrong
    // key, so nothing is selected and the user picks by hand.
    const known = knownSecretIds.current;
    if (known === null) return;
    try {
      const fresh = await queryClient.fetchQuery({
        queryKey: credentialKeys.secrets,
        queryFn: ipc.listSecrets,
        staleTime: 0,
      });
      const added = fresh.filter((entry) => !known.has(entry.id));
      const [created] = added;
      if (added.length === 1 && created) setSecretRef(created.id);
    } catch {
      // The list view shows its own error; the user can pick the secret by hand.
    }
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    const trimmedUrl = baseUrl.trim();
    const next: FormErrors = {
      displayName: displayName.trim() ? undefined : t('providers.form.displayNameRequired'),
      model: model.trim() ? undefined : t('providers.form.modelRequired'),
      baseUrl:
        !requiresCli && trimmedUrl && !isHttpUrl(trimmedUrl)
          ? t('providers.form.baseUrlInvalid')
          : undefined,
      secret: requiresApiKey && !secretRef ? t('providers.form.secretRequired') : undefined,
    };
    setErrors(next);
    if (next.displayName || next.model || next.baseUrl || next.secret) {
      setAttempt((count) => count + 1);
      return;
    }

    upsert.mutate(
      {
        id: profile?.id ?? null,
        kind,
        displayName: displayName.trim(),
        model: model.trim(),
        // The provider's own default needs no override.
        baseUrl:
          !requiresCli && trimmedUrl && trimmedUrl !== info?.defaultBaseUrl ? trimmedUrl : null,
        secretRef: requiresCli ? null : secretRef || null,
        supportsVision: isOllama ? supportsVision : false,
      },
      {
        onSuccess: () => {
          toast.success(t('providers.form.saved'));
          onClose();
        },
        onError: handleError,
      },
    );
  }

  const suggestions = modelSuggestions(info);
  const pending = upsert.isPending;
  const secretHint = vaultUnavailable
    ? t('providers.form.secretVaultUnavailable')
    : secrets.isError && !needsUnlock
      ? t('providers.form.secretsFailed')
      : requiresApiKey
        ? t('providers.form.secretHint')
        : t('providers.form.secretHintOptional');

  return (
    <>
      <Dialog
        open
        onClose={onClose}
        dismissible={!pending}
        title={profile ? t('providers.form.editTitle') : t('providers.form.addTitle')}
      >
        <form ref={formRef} className="dialog__form" onSubmit={submit} noValidate>
          <SelectField
            label={t('providers.form.kind')}
            value={kind}
            onChange={(event) => changeKind(event.target.value as ProviderKind)}
            disabled={profile !== null}
            hint={profile ? t('providers.form.kindLocked') : undefined}
          >
            {PROVIDER_KINDS.map((entry) => (
              <option key={entry} value={entry}>
                {kindLabel(entry, t)}
              </option>
            ))}
          </SelectField>

          {requiresCli ? (
            <p className="cli-note" role="note">
              <Trans i18nKey={cliNoteKey} components={{ code: <code /> }} />
            </p>
          ) : null}

          <TextField
            label={t('providers.form.displayName')}
            value={displayName}
            onChange={(event) => setDisplayName(event.target.value)}
            error={errors.displayName}
            autoComplete="off"
          />

          <div className="field">
            <TextField
              label={t('providers.form.model')}
              value={model}
              onChange={(event) => setModel(event.target.value)}
              error={errors.model}
              hint={t('providers.form.modelHint')}
              autoComplete="off"
              spellCheck={false}
            />
            <DeprecationNote kinds={kinds} kind={kind} model={model} />
            {suggestions.length > 0 ? (
              <div className="chip-row" role="group" aria-label={t('providers.form.suggestions')}>
                {suggestions.map(({ model: suggestion, tasks }) => (
                  <Button
                    key={suggestion}
                    size="sm"
                    className="model-chip"
                    onClick={() => setModel(suggestion)}
                    aria-pressed={model === suggestion}
                    title={tasks.map((task) => t(`providers.capabilities.${task}`)).join(', ')}
                  >
                    {suggestion}
                  </Button>
                ))}
              </div>
            ) : null}
          </div>

          {isOllama ? (
            <div className="field">
              <label className="check">
                <input
                  type="checkbox"
                  checked={supportsVision}
                  onChange={(event) => setSupportsVision(event.target.checked)}
                />
                <span>{t('providers.form.supportsVision')}</span>
              </label>
              <p className="field__hint">{t('providers.form.supportsVisionHint')}</p>
            </div>
          ) : null}

          {requiresCli ? null : (
            <>
              <TextField
                type="url"
                label={t('providers.form.baseUrl')}
                value={baseUrl}
                onChange={(event) => setBaseUrl(event.target.value)}
                error={errors.baseUrl}
                hint={t('providers.form.baseUrlHint', { url: info?.defaultBaseUrl ?? '' })}
                placeholder={info?.defaultBaseUrl}
                autoComplete="off"
                spellCheck={false}
              />

              <div className="field">
                <SelectField
                  label={
                    requiresApiKey
                      ? t('providers.form.secret')
                      : `${t('providers.form.secret')} (${t('providers.form.optional')})`
                  }
                  value={secretRef}
                  onChange={(event) => setSecretRef(event.target.value)}
                  error={errors.secret}
                  hint={secretHint}
                  disabled={vault.isPending}
                >
                  <option value="">
                    {requiresApiKey
                      ? t('providers.form.secretChoose')
                      : t('providers.form.secretNone')}
                  </option>
                  {secretList.map((entry) => (
                    <option key={entry.id} value={entry.id}>
                      {entry.name}
                    </option>
                  ))}
                  {secretRef !== '' && (secretMissing || !secretsReady) ? (
                    <option value={secretRef}>
                      {secretMissing
                        ? t('providers.form.secretGone')
                        : t('providers.form.secretUnlistable')}
                    </option>
                  ) : null}
                </SelectField>
                {secretMissing ? (
                  <p className="field__error" role="alert">
                    <WarningIcon className="field__error-icon" />
                    {t('providers.form.secretGoneWarning')}
                  </p>
                ) : null}
                {vault.isError ? (
                  <div className="alert" role="alert">
                    <span>{t('providers.form.vaultStatusFailed')}</span>
                    <Button size="sm" onClick={() => void vault.refetch()}>
                      {t('common.retry')}
                    </Button>
                  </div>
                ) : null}
                {needsUnlock ? (
                  <p className="field__hint">
                    {t('providers.form.vaultLocked')}{' '}
                    <Link to="/credentials" onClick={onClose}>
                      {t('providers.form.openCredentials')}
                    </Link>
                  </p>
                ) : null}
                <div>
                  <Button
                    size="sm"
                    onClick={() => {
                      knownSecretIds.current = secrets.data
                        ? new Set(secrets.data.map((entry) => entry.id))
                        : null;
                      setNewSecretOpen(true);
                    }}
                    disabled={vault.isPending || vault.isError || vaultUnavailable || needsUnlock}
                  >
                    {t('providers.form.newSecret')}
                  </Button>
                </div>
              </div>
            </>
          )}

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
      {/* A sibling, not a child: key events of the nested dialog must not reach this dialog's. */}
      <SecretDialog
        target={newSecretOpen ? 'new' : null}
        onClose={() => setNewSecretOpen(false)}
        onSaved={() => void handleSecretCreated()}
      />
    </>
  );
}

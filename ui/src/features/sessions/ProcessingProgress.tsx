import { useEffect, useId, useRef, useState, type ReactNode } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import type { TFunction } from 'i18next';
import { useTranslation } from 'react-i18next';
import { Link } from 'react-router';

import { Button } from '../../components/ui/Button';
import { ConfirmDialog } from '../../components/ui/ConfirmDialog';
import { TanIllustration } from '../../components/ui/TanIllustration';
import { events, type ProcessingProgressEvent } from '../../ipc/bindings';
import {
  ProcessingCommandError,
  TranscriptionCommandError,
  describeCommandError,
  useCommandErrorHandler,
} from '../../ipc/errors';
import { useTauriEvent } from '../../ipc/useTauriEvent';
import { useProfiles } from '../providers/api';
import { SelectField } from '../providers/SelectField';
import {
  useCancelProcessing,
  useIsProcessingRunPending,
  useLastProcessingError,
  useProcessingRun,
  useProcessingStatus,
  useTranscriptionEstimate,
  type ProcessingJob,
  type Session,
  type SessionDetail,
} from './api';
import { formatDuration } from './format';
import { sessionKeys } from './keys';
import { LONG_LOCAL_TRANSCRIPTION_SECONDS } from './longTranscription';

import '../../components/ui/ui.css';
import './sessions.css';

const STEPS = ['transcribe', 'describe_frames', 'synthesize'] as const;

/**
 * Everything about turning a recorded session into a note: the forecast and the start button
 * before, step progress with cancel while it runs, the reason with "try again" when it stopped,
 * and a re-process option once it is done.
 */
export function ProcessingProgress({ detail }: { detail: SessionDetail }) {
  const { t } = useTranslation();
  const { session } = detail;
  const runState = useRunState(session.id, detail.job);
  const { job, running, progress } = runState;
  const lastError = useLastProcessingError(session.id);

  if (session.status === 'recording') {
    return (
      <section className="processing" aria-label={t('processing.title')}>
        <p role="status">{t('processing.recordingInProgress')}</p>
      </section>
    );
  }

  if (running) {
    return (
      <RunningView
        sessionId={session.id}
        heading={t('processing.running')}
        job={job}
        progress={progress}
      />
    );
  }

  const failedStep = job?.steps.find((step) => step.state === 'failed');
  const interrupted = session.status === 'processing';
  const failed =
    session.status === 'failed' || failedStep !== undefined || lastError !== null || interrupted;

  if (failed) {
    const nothingCaptured =
      session.status === 'failed' &&
      job === null &&
      lastError === null &&
      detail.audioTracks.length === 0 &&
      detail.frames.length === 0;
    if (nothingCaptured) {
      return (
        <section className="processing" aria-label={t('processing.title')}>
          <p className="processing__note" role="status">
            {t('processing.nothingCaptured')}
          </p>
        </section>
      );
    }
    return (
      <FailedView
        sessionId={session.id}
        job={job}
        error={lastError}
        failedStepName={failedStep?.step ?? null}
        failedStepDetail={failedStep?.error ?? null}
        interrupted={interrupted && failedStep === undefined && lastError === null}
      />
    );
  }

  if (session.status === 'processed') {
    return <ProcessedView job={job} />;
  }

  return <ReadyView detail={detail} />;
}

// ---------------------------------------------------------------------------------------

/**
 * What is going on with a session's processing run: the job, whether a run is active and the
 * latest progress event. A run that ends refreshes the session, so every screen that shows this
 * state also picks up the new note and status.
 */
function useRunState(sessionId: string, fallbackJob: ProcessingJob | null) {
  const queryClient = useQueryClient();
  const status = useProcessingStatus(sessionId);
  const runPending = useIsProcessingRunPending(sessionId);
  const [progress, setProgress] = useState<ProcessingProgressEvent | null>(null);

  useTauriEvent(events.processingProgressEvent, (event) => {
    if (event.sessionId !== sessionId) return;
    // A new step began: re-read the job so the step list follows (also for a run started elsewhere).
    if (progress?.step !== event.step) {
      void queryClient.invalidateQueries({ queryKey: sessionKeys.processing(sessionId) });
    }
    setProgress(event);
  });

  const job = status.data?.job ?? fallbackJob;
  const running = status.data?.running === true || runPending;

  // The run ended (maybe started from another screen): pick up the note and the new status.
  const wasRunning = useRef(false);
  useEffect(() => {
    if (wasRunning.current && !running) {
      // The next run must not start on this run's last step and percentage.
      setProgress(null);
      void queryClient.invalidateQueries({ queryKey: sessionKeys.detail(sessionId) });
      void queryClient.invalidateQueries({ queryKey: [...sessionKeys.all, 'list'] });
    }
    wasRunning.current = running;
  }, [running, queryClient, sessionId]);

  return { job, running, progress };
}

/**
 * The card of a session that is being processed, for the library: it shows only while a run is
 * really active (a session left in `processing` by a closed app has nothing to show or cancel).
 */
export function LibraryProcessingCard({ session }: { session: Session }) {
  const { t } = useTranslation();
  const { job, running, progress } = useRunState(session.id, null);
  if (!running) return null;
  return (
    <RunningView
      sessionId={session.id}
      heading={
        <Link className="processing__session-link" to={`/recordings/${session.id}`}>
          {session.title || t('sessions.list.untitled')}
        </Link>
      }
      job={job}
      progress={progress}
    />
  );
}

/** Starts a full re-run of a processed session, after the user confirms it. */
export function ReprocessButton({
  sessionId,
  fallbackJob,
}: {
  sessionId: string;
  /** The job the detail came with; used until the live job status has loaded. */
  fallbackJob: ProcessingJob | null;
}) {
  const { t } = useTranslation();
  const run = useProcessingRun();
  const handleError = useCommandErrorHandler();
  const status = useProcessingStatus(sessionId);
  const lastError = useLastProcessingError(sessionId);
  const [confirming, setConfirming] = useState(false);
  const busy = run.isPending || status.data?.running === true;

  // A failed run is answered by "Tekrar dene" in the processing panel; two ways to start again
  // side by side would only confuse.
  const job = status.data?.job ?? fallbackJob;
  if (lastError !== null || job?.steps.some((step) => step.state === 'failed')) return null;

  return (
    <>
      <Button onClick={() => setConfirming(true)} disabled={busy}>
        {t('processing.reprocess')}
      </Button>
      <ConfirmDialog
        open={confirming}
        title={t('processing.reprocessTitle')}
        message={t('processing.reprocessMessage')}
        confirmLabel={t('processing.reprocess')}
        onCancel={() => setConfirming(false)}
        onConfirm={() => {
          setConfirming(false);
          run.mutate({ sessionId, mode: 'process' }, { onError: handleError });
        }}
      />
    </>
  );
}

function stepLabel(step: string | null, t: TFunction): string {
  return step ? t(`processing.steps.${step}`, { defaultValue: step }) : '';
}

function RunningView({
  sessionId,
  heading,
  job,
  progress,
}: {
  sessionId: string;
  heading: ReactNode;
  job: ProcessingJob | null;
  progress: ProcessingProgressEvent | null;
}) {
  const { t } = useTranslation();
  const handleError = useCommandErrorHandler();
  const cancel = useCancelProcessing();
  const headingId = useId();
  const runningStep = job?.steps.find((step) => step.state === 'running')?.step ?? null;
  const currentStep = progress?.step ?? runningStep;

  return (
    <section className="processing processing--with-tan" aria-labelledby={headingId}>
      <TanIllustration pose="focused" width={72} decorative className="processing__tan" />
      <div className="processing__body">
        <div className="processing__head">
          <h2 className="processing__title" id={headingId}>
            {heading}
          </h2>
          <p className="processing__percent">
            {progress
              ? t('processing.percent', { percent: Math.round(progress.percent) })
              : t('processing.starting')}
            {currentStep ? ` — ${stepLabel(currentStep, t)}` : ''}
          </p>
        </div>
        {/* Without a value the bar is indeterminate: no event has arrived since this screen opened. */}
        <progress
          max={100}
          value={progress ? progress.percent : undefined}
          aria-label={t('processing.progressLabel')}
        />
        <StepList job={job} />
      </div>
      <div className="processing__actions">
        <Button
          onClick={() => cancel.mutate(sessionId, { onError: handleError })}
          disabled={cancel.isPending}
        >
          {t('processing.cancel')}
        </Button>
      </div>
    </section>
  );
}

function StepList({ job }: { job: ProcessingJob | null }) {
  const { t } = useTranslation();
  const steps =
    job?.steps ?? STEPS.map((step) => ({ step, state: 'pending' as const, error: null }));
  return (
    <ol className="step-list" aria-label={t('processing.stepsLabel')}>
      {steps.map((entry, index) => (
        <li key={entry.step} className={`step-list__item step-list__item--${entry.state}`}>
          <span className="step-list__name">
            <span className="step-list__number" aria-hidden="true">
              {index + 1}
            </span>{' '}
            {stepLabel(entry.step, t)}
          </span>
          <span className="step-list__state">{t(`processing.stepState.${entry.state}`)}</span>
        </li>
      ))}
    </ol>
  );
}

function FailedView({
  sessionId,
  job,
  error,
  failedStepName,
  failedStepDetail,
  interrupted,
}: {
  sessionId: string;
  job: ProcessingJob | null;
  error: ProcessingCommandError | null;
  failedStepName: string | null;
  failedStepDetail: string | null;
  interrupted: boolean;
}) {
  const { t } = useTranslation();
  const run = useProcessingRun();
  const handleError = useCommandErrorHandler();

  const described = error ? describeCommandError(error, t) : null;
  const cancelled = error?.code === 'cancelled';
  const message = described
    ? described.message
    : interrupted
      ? t('processing.interrupted')
      : t('processing.failedAt', { step: stepLabel(failedStepName, t) });
  // The step's own text is English and key-free; it explains what the localised message cannot.
  const detail = described?.detail ?? (error === null ? failedStepDetail : null);

  const needsProvider = error?.code === 'no_chat_model';
  const needsModel =
    error?.code === 'transcription_failed' && error.transcriptionCode === 'local_model_missing';

  function retry() {
    run.mutate(
      { sessionId, mode: 'retry' },
      {
        // A failed run is shown inline (from the mutation cache), so only other failures toast.
        onError: (failure) => {
          if (!(failure instanceof ProcessingCommandError)) handleError(failure);
        },
      },
    );
  }

  return (
    <section className="processing processing--with-tan" aria-labelledby="processing-heading">
      <TanIllustration pose="thinking" width={72} decorative className="processing__tan" />
      <div className="processing__body">
        <h2 className="processing__title" id="processing-heading">
          {cancelled ? t('processing.cancelledTitle') : t('processing.failedTitle')}
        </h2>
        <div
          className={cancelled ? 'processing__note' : 'alert'}
          role={cancelled ? 'status' : 'alert'}
        >
          <div>
            <p>{message}</p>
            {detail ? <p className="processing__detail">{detail}</p> : null}
            {needsProvider ? (
              <p>
                <Link to="/providers">{t('processing.openProviders')}</Link>
              </p>
            ) : null}
            {needsModel ? (
              <p>
                <Link to="/settings">{t('processing.openSettings')}</Link>
              </p>
            ) : null}
          </div>
        </div>
        {job ? <StepList job={job} /> : null}
      </div>
      <div className="processing__actions">
        <Button variant="primary" onClick={retry} disabled={run.isPending}>
          {t('processing.retry')}
        </Button>
      </div>
    </section>
  );
}

function ProcessedView({ job }: { job: ProcessingJob | null }) {
  const { t } = useTranslation();
  const framesSkipped = job?.steps.some(
    (step) => step.step === 'describe_frames' && step.state === 'skipped',
  );
  // Re-processing is offered in the page header; this only explains what the run left out.
  if (!framesSkipped) return null;

  return (
    <section className="processing processing--done" aria-label={t('processing.title')}>
      <p className="processing__note" role="status">
        {t('processing.framesSkipped')} <Link to="/providers">{t('processing.openProviders')}</Link>
      </p>
    </section>
  );
}

function ReadyView({ detail }: { detail: SessionDetail }) {
  const { t } = useTranslation();
  const sessionId = detail.session.id;
  const run = useProcessingRun();
  const handleError = useCommandErrorHandler();
  const estimate = useTranscriptionEstimate(sessionId, true);
  const profiles = useProfiles();
  const [chosenProfile, setChosenProfile] = useState('');

  const forecast = estimate.data;
  const seconds = forecast?.estimatedSeconds ?? null;
  const longOnCpu =
    forecast?.engine === 'local_whisper' &&
    forecast.backend === 'cpu' &&
    seconds !== null &&
    seconds > LONG_LOCAL_TRANSCRIPTION_SECONDS;
  const cloudProfiles = (profiles.data ?? []).filter(
    (profile) => profile.capabilities.transcription,
  );
  const cloudProfileId =
    cloudProfiles.find((profile) => profile.id === chosenProfile)?.id ?? cloudProfiles[0]?.id ?? '';

  /** `transcriptionProfileId` makes the cloud profile transcribe this session only. */
  function start(transcriptionProfileId: string | null = null) {
    run.mutate(
      { sessionId, mode: 'process', transcriptionProfileId },
      {
        // A failed run is shown inline by the failed view, which replaces this one.
        onError: (failure) => {
          if (!(failure instanceof ProcessingCommandError)) handleError(failure);
        },
      },
    );
  }

  return (
    <section className="processing" aria-labelledby="processing-heading">
      <h2 className="processing__title" id="processing-heading">
        {t('processing.readyTitle')}
      </h2>
      <p className="processing__intro">{t('processing.readyIntro')}</p>

      {estimate.isPending ? (
        <p className="processing__estimate" role="status">
          {t('processing.estimating')}
        </p>
      ) : estimate.isError ? (
        <p className="processing__estimate" role="status">
          {describeCommandError(estimate.error, t).message}
        </p>
      ) : forecast ? (
        <p className="processing__estimate" role="status">
          {forecast.engine === 'provider'
            ? t('processing.estimateCloud')
            : seconds !== null
              ? t('processing.estimateLocal', {
                  duration: formatDuration(seconds * 1000, t),
                  backend: forecast.backend,
                })
              : t('processing.estimateUnknown')}
        </p>
      ) : null}

      {longOnCpu ? (
        <div className="processing__suggestion" role="note">
          <p>
            {t('processing.longOnCpu', {
              duration: seconds === null ? '' : formatDuration(seconds * 1000, t),
            })}
          </p>
          {cloudProfiles.length > 0 ? (
            <>
              {cloudProfiles.length > 1 ? (
                <SelectField
                  label={t('processing.cloudProfile')}
                  value={cloudProfileId}
                  onChange={(event) => setChosenProfile(event.target.value)}
                >
                  {cloudProfiles.map((profile) => (
                    <option key={profile.id} value={profile.id}>
                      {`${profile.displayName} (${profile.model})`}
                    </option>
                  ))}
                </SelectField>
              ) : (
                <p>
                  {t('processing.cloudProfileSingle', {
                    name: cloudProfiles[0]?.displayName ?? '',
                  })}
                </p>
              )}
              <p className="processing__hint">{t('processing.cloudAssignmentNote')}</p>
              <div>
                <Button
                  variant="primary"
                  onClick={() => start(cloudProfileId)}
                  disabled={run.isPending || !cloudProfileId}
                >
                  {t('processing.processInCloud')}
                </Button>
              </div>
            </>
          ) : (
            <p>
              <Link to="/providers">{t('processing.addCloudProfile')}</Link>
            </p>
          )}
        </div>
      ) : null}

      {estimate.error instanceof TranscriptionCommandError &&
      estimate.error.code === 'local_model_missing' ? (
        <p>
          <Link to="/settings">{t('processing.openSettings')}</Link>
        </p>
      ) : null}

      <div className="processing__actions">
        <Button
          variant={longOnCpu ? 'secondary' : 'primary'}
          onClick={() => start()}
          disabled={run.isPending}
        >
          {longOnCpu ? t('processing.processLocal') : t('processing.process')}
        </Button>
      </div>
    </section>
  );
}

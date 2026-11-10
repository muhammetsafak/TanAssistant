import { useState, type FormEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router';

import { Button } from '../../components/ui/Button';
import { Chip } from '../../components/ui/Chip';
import { TanIllustration } from '../../components/ui/TanIllustration';
import { TextField } from '../../components/ui/TextField';
import { useToast } from '../../components/ui/toast-context';
import { useCommandErrorHandler } from '../../ipc/errors';
import { SelectField } from '../providers/SelectField';
import { AUTO_LANGUAGE, languageOptions } from '../settings/languages';
import { useSettings } from '../settings/useSettings';
import { formatClock } from '../sessions/format';
import {
  useAudioDevices,
  useDisplays,
  usePauseRecording,
  useRecordingCapabilities,
  useRecordingStatus,
  useResumeRecording,
  useStartRecording,
  useStopRecording,
  type ActiveRecording,
  type RecordingSummary,
} from './api';
import { PauseControlIcon, PlayIcon, RecordIcon, StopIcon } from './ControlIcons';
import { DisplayPicker } from './DisplayPicker';
import { LevelMeter } from './LevelMeter';
import { useRecordingEvents } from './useRecordingEvents';

import '../../components/ui/ui.css';
import './recording.css';

/** The recording screen: the start form while idle, the live view while recording. */
export function RecordingControl() {
  const { t } = useTranslation();
  const status = useRecordingStatus();

  return (
    <section className="recording-screen" aria-labelledby="recording-heading">
      {status.isPending ? (
        <div className="recording-panel">
          <h2 className="sr-only" id="recording-heading">
            {t('recording.title')}
          </h2>
          <p role="status">{t('common.loading')}</p>
        </div>
      ) : status.isError ? (
        <div className="recording-panel">
          <h2 className="sr-only" id="recording-heading">
            {t('recording.title')}
          </h2>
          <div className="recording-failure">
            <TanIllustration pose="thinking" width={56} decorative />
            <div className="alert" role="alert">
              <span>{t('recording.statusFailed')}</span>
              <Button size="sm" onClick={() => void status.refetch()}>
                {t('common.retry')}
              </Button>
            </div>
          </div>
        </div>
      ) : (status.data.state === 'recording' || status.data.state === 'paused') &&
        status.data.active ? (
        <ActiveRecordingView active={status.data.active} paused={status.data.state === 'paused'} />
      ) : (
        <StartForm />
      )}
    </section>
  );
}

function StartForm() {
  const { t } = useTranslation();
  const handleError = useCommandErrorHandler();
  const settings = useSettings();
  const capabilities = useRecordingCapabilities();
  const devices = useAudioDevices(true);
  const screenAvailable = capabilities.data?.screenCaptureAvailable === true;
  const displays = useDisplays(screenAvailable);
  const start = useStartRecording();

  const [title, setTitle] = useState('');
  // `null` follows the default from the settings until the user picks something for this recording.
  const [speechLanguage, setSpeechLanguage] = useState<string | null>(null);
  const [noteLanguage, setNoteLanguage] = useState<string | null>(null);
  const [microphoneId, setMicrophoneId] = useState('');
  const [displayId, setDisplayId] = useState('');

  const speech = speechLanguage ?? settings.data?.defaultSpeechLanguage ?? AUTO_LANGUAGE;
  const note = noteLanguage ?? settings.data?.defaultNoteLanguage ?? 'tr';

  function submit(event: FormEvent) {
    event.preventDefault();
    start.mutate(
      {
        title: title.trim() || null,
        speechLanguage: speech,
        noteLanguage: note,
        microphoneDeviceId: microphoneId || null,
        displayId: screenAvailable && displayId ? displayId : null,
      },
      { onError: handleError },
    );
  }

  const unavailableReason = capabilities.data?.screenUnavailableReason;
  // The idle level preview names the microphone that will be used: the chosen one, else the default.
  const microphoneName =
    (devices.data ?? []).find((device) =>
      microphoneId ? device.id === microphoneId : device.isDefault,
    )?.name ?? t('recording.track.microphone');

  return (
    <form className="recording-layout" onSubmit={submit} aria-labelledby="recording-heading">
      <div className="recording-panel">
        <h2 className="sr-only" id="recording-heading">
          {t('recording.newTitle')}
        </h2>

        <div className="recording-panel__top">
          <Timer value={formatClock(0)} hint={t('recording.hintIdle')} mode="idle" />
          <TanIllustration pose="sitting-desk" width={132} className="recording-panel__tan" />
        </div>

        <div className="recording-meters">
          <LevelMeter
            name={t('recording.meterMe')}
            source={microphoneName}
            tone="me"
            level={null}
            mode="idle"
          />
          <LevelMeter
            name={t('recording.meterThem')}
            source={t('recording.track.system')}
            tone="them"
            level={null}
            mode="idle"
          />
        </div>

        <div className="recording-form__grid">
          <TextField
            label={t('recording.titleLabel')}
            hint={t('recording.titleHint')}
            value={title}
            onChange={(event) => setTitle(event.target.value)}
            maxLength={200}
            autoComplete="off"
          />

          <SelectField
            label={t('recording.speechLanguage')}
            value={speech}
            onChange={(event) => setSpeechLanguage(event.target.value)}
          >
            {languageOptions(speech, true).map((code) => (
              <option key={code} value={code}>
                {t(`languageNames.${code}`, { defaultValue: code })}
              </option>
            ))}
          </SelectField>

          <SelectField
            label={t('recording.noteLanguage')}
            value={note}
            onChange={(event) => setNoteLanguage(event.target.value)}
            hint={t('recording.noteLanguageHint')}
          >
            {languageOptions(note, false).map((code) => (
              <option key={code} value={code}>
                {t(`languageNames.${code}`, { defaultValue: code })}
              </option>
            ))}
          </SelectField>

          <SelectField
            label={t('recording.microphone')}
            value={microphoneId}
            onChange={(event) => setMicrophoneId(event.target.value)}
            aria-busy={devices.isPending}
            hint={
              devices.isError
                ? t('recording.devicesFailed')
                : devices.data?.length === 0
                  ? t('recording.noDevices')
                  : undefined
            }
          >
            <option value="">{t('recording.microphoneDefault')}</option>
            {(devices.data ?? []).map((device) => (
              <option key={device.id} value={device.id}>
                {device.isDefault
                  ? `${device.name} (${t('recording.systemDefault')})`
                  : device.name}
              </option>
            ))}
          </SelectField>
        </div>

        {capabilities.isError ? (
          <p className="recording-note" role="status">
            {t('recording.capabilitiesFailed')}
          </p>
        ) : capabilities.data && !screenAvailable ? (
          <p className="recording-note" role="status">
            {unavailableReason
              ? t(`recording.screenUnavailable.${unavailableReason}`)
              : t('recording.screenUnavailable.generic')}
          </p>
        ) : null}

        <div className="recording-form__actions">
          <Button type="submit" variant="live" disabled={start.isPending}>
            <RecordIcon />
            {start.isPending ? t('recording.starting') : t('recording.start')}
          </Button>
        </div>
      </div>

      {screenAvailable ? (
        <div className="recording-panel recording-panel--side">
          <DisplayPicker
            displays={displays.data ?? []}
            value={displayId}
            onChange={setDisplayId}
            loading={displays.isPending}
            failed={displays.isError}
          />
        </div>
      ) : null}
    </form>
  );
}

/** The clock of the panel: a label, the big mono time and a line saying what it is doing. */
function Timer({
  value,
  hint,
  mode,
}: {
  value: string;
  hint: string;
  mode: 'idle' | 'live' | 'paused';
}) {
  const { t } = useTranslation();
  return (
    <div className="recording-clock">
      <dl>
        <dt className="recording-clock__label">{t('recording.elapsed')}</dt>
        <dd
          role={mode === 'idle' ? undefined : 'timer'}
          className={`recording-timer recording-timer--${mode}`}
        >
          {value}
        </dd>
      </dl>
      <p className="recording-clock__hint">{hint}</p>
    </div>
  );
}

/** Tracks whose source delivered only digital silence (peak 0) are worth a warning. */
function silentTracks(summary: RecordingSummary): string[] {
  return summary.tracks.filter((track) => track.peak === 0).map((track) => track.kind);
}

function ActiveRecordingView({ active, paused }: { active: ActiveRecording; paused: boolean }) {
  const { t } = useTranslation();
  const toast = useToast();
  const navigate = useNavigate();
  const handleError = useCommandErrorHandler();
  const stop = useStopRecording();
  const pause = usePauseRecording();
  const resume = useResumeRecording();
  const { live, warnings } = useRecordingEvents();

  // The status read is a snapshot; the progress ticks that follow carry the live numbers. A paused
  // recording has none: a tick that was already on its way must not move the clock.
  const current = !paused && live?.sessionId === active.sessionId ? live : active;
  const switching = pause.isPending || resume.isPending;

  function togglePause() {
    (paused ? resume : pause).mutate(undefined, { onError: handleError });
  }

  async function stopRecording() {
    try {
      const summary = await stop.mutateAsync();
      if (summary.status === 'failed') {
        toast.errorText(t('recording.nothingCaptured'));
        return;
      }
      for (const kind of silentTracks(summary)) {
        toast.errorText(t('recording.silentTrack', { track: t(`recording.track.${kind}`) }));
      }
      if (summary.warnings.length > 0) {
        toast.errorText(
          `${t('recording.warningsAfterStop')} ${summary.warnings.map((w) => w.message).join(' · ')}`,
        );
      }
      void navigate(`/recordings/${summary.sessionId}`);
    } catch (error) {
      handleError(error);
    }
  }

  const mode = paused ? 'paused' : 'live';

  return (
    <div className="recording-layout">
      <div className={`recording-panel recording-panel--${mode}`}>
        <h2 className="sr-only" id="recording-heading">
          {paused ? t('recording.pausedTitle') : t('recording.active')}
        </h2>

        <div className="recording-panel__top">
          <Timer
            value={formatClock(current.elapsedMs)}
            hint={paused ? t('recording.hintPaused') : t('recording.hintLive')}
            mode={mode}
          />
          <div className="recording-panel__aside">
            {paused ? (
              <Chip kind="paused">{t('recording.paused')}</Chip>
            ) : (
              <Chip kind="live">{t('recording.live')}</Chip>
            )}
            <dl className="recording-frames">
              <dt>{t('recording.frames')}</dt>
              <dd>{current.frameCount}</dd>
            </dl>
          </div>
        </div>

        <p className="recording-active__name">{active.title}</p>

        {paused ? (
          <p className="recording-note" role="status">
            {t('recording.pausedNote')}
          </p>
        ) : null}

        <div className="recording-meters">
          <LevelMeter
            name={t('recording.meterMe')}
            source={t('recording.track.microphone')}
            tone="me"
            level={current.microphoneLevel}
            mode={mode}
          />
          <LevelMeter
            name={t('recording.meterThem')}
            source={t('recording.track.system')}
            tone="them"
            level={current.systemLevel}
            mode={mode}
          />
        </div>

        {warnings.length > 0 ? (
          <div className="alert recording-warnings" role="status">
            <div>
              <p className="recording-warnings__title">{t('recording.warningsTitle')}</p>
              <ul>
                {warnings.map((message) => (
                  <li key={message}>{message}</li>
                ))}
              </ul>
            </div>
          </div>
        ) : null}

        <div className="recording-form__actions">
          <Button
            variant={paused ? 'live' : 'secondary'}
            className={paused ? undefined : 'recording-pause'}
            onClick={togglePause}
            disabled={switching || stop.isPending}
            aria-label={paused ? t('recording.resumeLabel') : t('recording.pauseLabel')}
            aria-busy={switching}
          >
            {paused ? <PlayIcon /> : <PauseControlIcon />}
            {paused ? t('recording.resume') : t('recording.pause')}
          </Button>
          <Button
            variant="secondary"
            onClick={() => void stopRecording()}
            disabled={stop.isPending || switching}
          >
            <StopIcon />
            {stop.isPending ? t('recording.stopping') : t('recording.stop')}
          </Button>
        </div>
      </div>
    </div>
  );
}

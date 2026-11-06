import { useState, type FormEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router';

import { Button } from '../../components/ui/Button';
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
  useRecordingCapabilities,
  useRecordingStatus,
  useStartRecording,
  useStopRecording,
  type ActiveRecording,
  type RecordingSummary,
} from './api';
import { LevelMeter } from './LevelMeter';
import { useRecordingEvents } from './useRecordingEvents';

import '../../components/ui/ui.css';
import './recording.css';

/** The recording card: the start form while idle, the live view while recording. */
export function RecordingControl() {
  const { t } = useTranslation();
  const status = useRecordingStatus();

  return (
    <section className="recording-card" aria-labelledby="recording-heading">
      {status.isPending ? (
        <>
          <h2 className="recording-card__title" id="recording-heading">
            {t('recording.title')}
          </h2>
          <p role="status">{t('common.loading')}</p>
        </>
      ) : status.isError ? (
        <>
          <h2 className="recording-card__title" id="recording-heading">
            {t('recording.title')}
          </h2>
          <div className="alert" role="alert">
            <span>{t('recording.statusFailed')}</span>
            <Button size="sm" onClick={() => void status.refetch()}>
              {t('common.retry')}
            </Button>
          </div>
        </>
      ) : status.data.state === 'recording' && status.data.active ? (
        <ActiveRecordingView active={status.data.active} />
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

  return (
    <form className="recording-form" onSubmit={submit} aria-labelledby="recording-heading">
      <h2 className="recording-card__title" id="recording-heading">
        {t('recording.newTitle')}
      </h2>

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
              {device.isDefault ? `${device.name} (${t('recording.systemDefault')})` : device.name}
            </option>
          ))}
        </SelectField>

        {screenAvailable ? (
          <SelectField
            label={t('recording.display')}
            value={displayId}
            onChange={(event) => setDisplayId(event.target.value)}
            aria-busy={displays.isPending}
            hint={displays.isError ? t('recording.displaysFailed') : undefined}
          >
            <option value="">{t('recording.displayDefault')}</option>
            {(displays.data ?? []).map((display) => (
              <option key={display.id} value={display.id}>
                {`${display.name} (${display.width}×${display.height})${
                  display.isPrimary ? `, ${t('recording.primaryDisplay')}` : ''
                }`}
              </option>
            ))}
          </SelectField>
        ) : null}
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
        <Button type="submit" variant="primary" disabled={start.isPending}>
          {start.isPending ? t('recording.starting') : t('recording.start')}
        </Button>
      </div>
    </form>
  );
}

/** Tracks whose source delivered only digital silence (peak 0) are worth a warning. */
function silentTracks(summary: RecordingSummary): string[] {
  return summary.tracks.filter((track) => track.peak === 0).map((track) => track.kind);
}

function ActiveRecordingView({ active }: { active: ActiveRecording }) {
  const { t } = useTranslation();
  const toast = useToast();
  const navigate = useNavigate();
  const handleError = useCommandErrorHandler();
  const stop = useStopRecording();
  const { live, warnings } = useRecordingEvents();

  // The status read is a snapshot; the progress ticks that follow carry the live numbers.
  const current = live?.sessionId === active.sessionId ? live : active;

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
        toast.errorText(`${t('recording.warningsAfterStop')} ${summary.warnings.join(' · ')}`);
      }
      void navigate(`/recordings/${summary.sessionId}`);
    } catch (error) {
      handleError(error);
    }
  }

  return (
    <div className="recording-active">
      <div className="recording-active__header">
        <h2 className="recording-card__title" id="recording-heading">
          <span className="recording-dot" aria-hidden="true" />
          {t('recording.active')}
        </h2>
        <p className="recording-active__name">{active.title}</p>
      </div>

      <dl className="recording-stats">
        <div>
          <dt>{t('recording.elapsed')}</dt>
          <dd role="timer" className="recording-timer">
            {formatClock(current.elapsedMs)}
          </dd>
        </div>
        <div>
          <dt>{t('recording.frames')}</dt>
          <dd>{current.frameCount}</dd>
        </div>
      </dl>

      <div className="recording-meters">
        <LevelMeter label={t('recording.track.microphone')} level={current.microphoneLevel} />
        <LevelMeter label={t('recording.track.system')} level={current.systemLevel} />
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
        <Button variant="danger" onClick={() => void stopRecording()} disabled={stop.isPending}>
          {stop.isPending ? t('recording.stopping') : t('recording.stop')}
        </Button>
      </div>
    </div>
  );
}

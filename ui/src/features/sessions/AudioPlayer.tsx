import { useEffect, useImperativeHandle, useMemo, useRef, useState, type Ref } from 'react';
import { useTranslation } from 'react-i18next';

import { Button } from '../../components/ui/Button';
import { audioUrl, type AudioTrack } from './api';
import { formatClock } from './format';
import type { Playhead } from './playhead';

import '../../components/ui/ui.css';
import './sessions.css';

/** What the page can ask of the player. */
export interface AudioPlayerHandle {
  /** Moves every track to `ms` (clamped to the recording). It does not start or stop playback. */
  seek: (ms: number) => void;
}

/** The other track may drift this far from the main clock before it is put back in step. */
const MAX_DRIFT_SECONDS = 0.15;

interface Source {
  key: string;
  kind: string;
  url: string;
  durationMs: number;
}

/** A follower that cannot play is not an error to raise: the element reports it as an `error` event. */
function ignoreRejection(): void {
  /* Nothing to do here. */
}

/**
 * The player of a session's audio tracks. Each track is a hidden `<audio>`; the microphone (or the
 * only track there is) is the main clock and the other one follows it. There is one set of
 * controls: play/pause, a position slider, the time, and a mute switch per track.
 */
export function AudioPlayer({
  sessionDir,
  tracks,
  playhead,
  onAvailabilityChange,
  ref,
}: {
  sessionDir: string;
  tracks: readonly AudioTrack[];
  /** Told the position as it moves, so other parts of the page can follow along. */
  playhead?: Playhead;
  /** `false` while no track can be played (none loads, or all failed): nothing can be moved then. */
  onAvailabilityChange?: (available: boolean) => void;
  ref?: Ref<AudioPlayerHandle>;
}) {
  const { t } = useTranslation();
  const elements = useRef(new Map<string, HTMLAudioElement>());
  const [playing, setPlaying] = useState(false);
  const [positionMs, setPositionMs] = useState(0);
  const [loadedMs, setLoadedMs] = useState(0);
  const [muted, setMuted] = useState<ReadonlySet<string>>(new Set());
  const [failed, setFailed] = useState<ReadonlySet<string>>(new Set());
  const [playFailed, setPlayFailed] = useState(false);

  const sources = useMemo<Source[]>(
    () =>
      tracks.flatMap((track) => {
        const url = audioUrl(sessionDir, track.path);
        return url === null
          ? []
          : [
              {
                key: `${track.kind}:${track.path}`,
                kind: track.kind,
                url,
                durationMs: track.durationMs,
              },
            ];
      }),
    [sessionDir, tracks],
  );
  const active = sources.filter((source) => !failed.has(source.key));
  const unavailable = tracks.length > 0 && active.length === 0;
  const mainKey = (active.find((source) => source.kind === 'microphone') ?? active[0])?.key;

  const trackMaxMs = Math.max(0, ...active.map((source) => source.durationMs));
  const totalMs = trackMaxMs > 0 ? trackMaxMs : loadedMs;

  const activeElements = () =>
    active.flatMap((source) => {
      const element = elements.current.get(source.key);
      return element ? [element] : [];
    });

  function report(ms: number) {
    setPositionMs(ms);
    playhead?.set(ms);
  }

  useEffect(() => {
    onAvailabilityChange?.(!unavailable);
  }, [unavailable, onAvailabilityChange]);

  function seek(ms: number) {
    // With nothing to play, moving the position would only pretend that something happened.
    if (activeElements().length === 0) return;
    const clamped = Math.min(Math.max(0, ms), totalMs);
    for (const element of activeElements()) element.currentTime = clamped / 1000;
    report(clamped);
  }
  // No dependency list: the handle always reaches the current tracks.
  useImperativeHandle(ref, () => ({ seek }));

  function pauseAll() {
    for (const element of activeElements()) element.pause();
    setPlaying(false);
  }

  async function togglePlay() {
    if (playing) {
      pauseAll();
      return;
    }
    const all = activeElements();
    if (all.length === 0) return;
    // Playing again from the end starts over.
    if (totalMs > 0 && positionMs >= totalMs - 50) seek(0);
    setPlayFailed(false);
    try {
      await Promise.all(all.map((element) => Promise.resolve(element.play())));
      setPlaying(true);
    } catch {
      for (const element of all) element.pause();
      setPlaying(false);
      setPlayFailed(true);
    }
  }

  /** The main clock moved: report it and bring the other track back in step when it drifted. */
  function handleMainTimeUpdate() {
    const main = mainKey ? elements.current.get(mainKey) : undefined;
    if (!main) return;
    report(main.currentTime * 1000);
    for (const source of active) {
      if (source.key === mainKey) continue;
      const follower = elements.current.get(source.key);
      if (!follower) continue;
      // A shorter track has simply ended; there is nothing to follow.
      const length = Number.isFinite(follower.duration) ? follower.duration : Infinity;
      if (main.currentTime >= length) continue;
      if (Math.abs(follower.currentTime - main.currentTime) > MAX_DRIFT_SECONDS) {
        follower.currentTime = main.currentTime;
      }
      if (!main.paused && follower.paused) Promise.resolve(follower.play()).catch(ignoreRejection);
    }
  }

  function handleMainMetadata() {
    const main = mainKey ? elements.current.get(mainKey) : undefined;
    if (main && Number.isFinite(main.duration)) setLoadedMs(main.duration * 1000);
  }

  function toggleMute(key: string) {
    setMuted((previous) => {
      const next = new Set(previous);
      if (!next.delete(key)) next.add(key);
      return next;
    });
  }

  function markFailed(key: string) {
    setFailed((previous) => new Set(previous).add(key));
  }

  function trackName(kind: string): string {
    return t(`sessions.audio.tracks.${kind}`, { defaultValue: kind });
  }

  function muteLabel(kind: string): string {
    if (kind === 'microphone') return t('sessions.audio.muteMicrophone');
    if (kind === 'system') return t('sessions.audio.muteSystem');
    return t('sessions.audio.muteOther', { track: kind });
  }

  if (tracks.length > 0 && sources.length === 0) {
    return (
      <p className="audio-player__problem" role="alert">
        {t('sessions.audio.unavailable')}
      </p>
    );
  }

  const failedSources = sources.filter((source) => failed.has(source.key));

  return (
    <div className="audio-player" role="group" aria-label={t('sessions.audio.playerLabel')}>
      {active.map((source) => (
        <audio
          key={source.key}
          ref={(element) => {
            if (element) elements.current.set(source.key, element);
            else elements.current.delete(source.key);
          }}
          src={source.url}
          preload="metadata"
          muted={muted.has(source.key)}
          onError={() => markFailed(source.key)}
          onTimeUpdate={source.key === mainKey ? handleMainTimeUpdate : undefined}
          onSeeked={source.key === mainKey ? handleMainTimeUpdate : undefined}
          onEnded={source.key === mainKey ? pauseAll : undefined}
          onPause={source.key === mainKey ? pauseAll : undefined}
          onLoadedMetadata={source.key === mainKey ? handleMainMetadata : undefined}
        />
      ))}

      <div className="audio-player__controls">
        <Button
          variant="primary"
          size="sm"
          className="audio-player__play"
          onClick={() => void togglePlay()}
          disabled={unavailable}
        >
          {playing ? t('sessions.audio.pause') : t('sessions.audio.play')}
        </Button>
        <input
          type="range"
          className="audio-player__slider"
          aria-label={t('sessions.audio.position')}
          aria-valuetext={t('sessions.audio.positionValue', {
            current: formatClock(positionMs),
            total: formatClock(totalMs),
          })}
          min={0}
          max={totalMs}
          step={1000}
          value={Math.min(positionMs, totalMs)}
          disabled={unavailable || totalMs === 0}
          onChange={(event) => seek(Number(event.currentTarget.value))}
        />
        <span className="audio-player__time" aria-hidden="true">
          {formatClock(positionMs)} / {formatClock(totalMs)}
        </span>
      </div>

      <div className="audio-player__tracks">
        {active.map((source) => {
          const isMuted = muted.has(source.key);
          return (
            <Button
              key={source.key}
              size="sm"
              className="audio-player__mute"
              aria-pressed={isMuted}
              aria-label={muteLabel(source.kind)}
              onClick={() => toggleMute(source.key)}
            >
              {trackName(source.kind)}
              {isMuted ? (
                <span className="audio-player__muted">{t('sessions.audio.muted')}</span>
              ) : null}
            </Button>
          );
        })}
      </div>

      {playFailed ? (
        <p className="audio-player__problem" role="alert">
          {t('sessions.audio.playFailed')}
        </p>
      ) : null}
      {failedSources.map((source) => (
        <p key={source.key} className="audio-player__problem" role="status">
          {t('sessions.audio.trackUnavailable', { track: trackName(source.kind) })}
        </p>
      ))}
    </div>
  );
}

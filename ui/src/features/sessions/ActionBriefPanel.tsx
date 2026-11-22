import { useCallback, useMemo, useState, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { Link } from 'react-router';

import { Button } from '../../components/ui/Button';
import { Dialog } from '../../components/ui/Dialog';
import { useToast } from '../../components/ui/toast-context';
import { describeCommandError } from '../../ipc/errors';
import {
  BriefCommandError,
  useActionBrief,
  useActionBriefMarkdown,
  useGenerateActionBrief,
  useIsGeneratingBrief,
  type ActionBrief,
  type ActionItem,
  type Frame,
} from './api';
import { formatClock, formatDateTime } from './format';
import { FrameImage } from './FrameImage';
import { FrameViewer } from './FrameViewer';

import './sessions.css';

export interface ActionBriefPanelProps {
  item: ActionItem;
  onClose: () => void;
  sessionDir: string;
  /** Every frame of the session; the brief names its frames by id. */
  frames: readonly Frame[];
  /** Given when there is audio to play: moves the player to a time, in milliseconds. */
  onSeek?: (ms: number) => void;
}

/**
 * The brief of one action item in a drawer: what the meeting said about it, the frames and quotes
 * that matter, and the steps the AI suggests. It is read only; the user can generate it, generate
 * it again (which replaces it) and copy it as Markdown.
 */
export function ActionBriefPanel({
  item,
  onClose,
  sessionDir,
  frames,
  onSeek,
}: ActionBriefPanelProps) {
  const { t } = useTranslation();
  const toast = useToast();
  const brief = useActionBrief(item.id, true);
  const generating = useIsGeneratingBrief(item.id);
  const markdown = useActionBriefMarkdown();

  const showError = useCallback(
    (error: unknown) => {
      const { message, detail } = describeBriefError(error, t);
      toast.errorText(detail ? `${message} (${detail})` : message);
    },
    [t, toast],
  );
  const generate = useGenerateActionBrief(item.id, showError);
  const noProvider =
    generate.error instanceof BriefCommandError && generate.error.code === 'no_provider';

  function copyMarkdown() {
    markdown.mutate(item.id, {
      onSuccess: async (text) => {
        try {
          await navigator.clipboard.writeText(text);
          toast.success(t('sessions.brief.copied'));
        } catch {
          toast.errorText(t('sessions.brief.copyFailed'));
        }
      },
      onError: showError,
    });
  }

  const data = brief.data ?? null;

  // A frame the session no longer has (the brief is older than a clean-up) is left out.
  const briefFrames = useMemo<BriefFrame[]>(
    () =>
      (data?.frames ?? []).flatMap((entry) => {
        const frame = frames.find((candidate) => candidate.id === entry.frameId);
        return frame ? [{ frame, reason: entry.reason }] : [];
      }),
    [data?.frames, frames],
  );
  const viewerFrames = useMemo(() => briefFrames.map((entry) => entry.frame), [briefFrames]);
  const [viewing, setViewing] = useState<number | null>(null);

  const notice = noProvider ? (
    <p className="alert" role="alert">
      <span>
        {t('sessions.brief.noProvider')}{' '}
        <Link to="/providers">{t('sessions.brief.noProviderLink')}</Link>
      </span>
    </p>
  ) : null;

  return (
    <>
      <Dialog
        open
        variant="drawer"
        onClose={onClose}
        title={t('sessions.brief.title')}
        description={item.task}
        footer={
          <>
            {data ? (
              <>
                <Button onClick={() => generate.mutate()} disabled={generating}>
                  {t('sessions.brief.regenerate')}
                </Button>
                <Button
                  variant="primary"
                  onClick={copyMarkdown}
                  disabled={markdown.isPending || generating}
                >
                  {t('sessions.brief.copy')}
                </Button>
              </>
            ) : null}
            <Button data-autofocus onClick={onClose}>
              {t('common.close')}
            </Button>
          </>
        }
      >
        <div className="brief" aria-busy={generating || brief.isPending}>
          {generating ? (
            <div className="brief__progress" role="status">
              <p>{t('sessions.brief.generating')}</p>
              <progress aria-label={t('sessions.brief.generating')} />
              <p className="field__hint">{t('sessions.brief.generatingHint')}</p>
            </div>
          ) : null}

          {brief.isPending ? (
            <p role="status">{t('common.loading')}</p>
          ) : brief.isError ? (
            <div className="alert" role="alert">
              <span>{t('sessions.brief.loadFailed')}</span>
              <Button size="sm" onClick={() => void brief.refetch()}>
                {t('common.retry')}
              </Button>
            </div>
          ) : data ? (
            <BriefContent
              brief={data}
              sessionDir={sessionDir}
              briefFrames={briefFrames}
              onOpenFrame={setViewing}
              onSeek={onSeek}
            />
          ) : generating ? null : (
            <div className="brief__empty">
              <p>{t('sessions.brief.none')}</p>
              <p className="field__hint">{t('sessions.brief.noneHint')}</p>
              {notice}
              <div>
                <Button variant="primary" onClick={() => generate.mutate()}>
                  {t('sessions.brief.generate')}
                </Button>
              </div>
            </div>
          )}

          {data ? notice : null}
        </div>
      </Dialog>
      <FrameViewer
        sessionDir={sessionDir}
        frames={viewerFrames}
        index={viewing}
        onIndexChange={setViewing}
        onClose={() => setViewing(null)}
      />
    </>
  );
}

/** Failure text chosen by the brief error's code; anything else goes through the common path. */
function describeBriefError(error: unknown, t: ReturnType<typeof useTranslation>['t']) {
  if (error instanceof BriefCommandError) {
    const message = t(`sessions.brief.errors.${error.code}`, { defaultValue: error.message });
    return message === error.message ? { message } : { message, detail: error.message };
  }
  return describeCommandError(error, t);
}

interface BriefFrame {
  frame: Frame;
  reason: string;
}

function BriefContent({
  brief,
  sessionDir,
  briefFrames,
  onOpenFrame,
  onSeek,
}: {
  brief: ActionBrief;
  sessionDir: string;
  briefFrames: readonly BriefFrame[];
  onOpenFrame: (index: number) => void;
  onSeek?: (ms: number) => void;
}) {
  const { t, i18n } = useTranslation();
  const toast = useToast();

  function seek(ms: number) {
    onSeek?.(ms);
    toast.info(t('sessions.brief.seeked', { time: formatClock(ms) }));
  }

  return (
    <>
      {brief.createdAt !== null ? (
        <p className="brief__meta">
          {t('sessions.brief.createdAt', { date: formatDateTime(brief.createdAt, i18n.language) })}
        </p>
      ) : null}

      {brief.context ? (
        <BriefSection id="context" title={t('sessions.brief.context')}>
          <p className="brief__text">{brief.context}</p>
        </BriefSection>
      ) : null}

      <BriefList id="details" title={t('sessions.brief.details')} items={brief.details} />
      <BriefList
        id="decisions"
        title={t('sessions.brief.relatedDecisions')}
        items={brief.relatedDecisions}
      />
      <BriefList
        id="questions"
        title={t('sessions.brief.openQuestions')}
        items={brief.openQuestions}
      />

      {briefFrames.length > 0 ? (
        <BriefSection id="frames" title={t('sessions.brief.frames')}>
          <ul className="brief-frames">
            {briefFrames.map(({ frame, reason }, index) => (
              <li key={frame.id} className="frame-card">
                <button
                  type="button"
                  className="frame-card__open"
                  onClick={() => onOpenFrame(index)}
                  aria-label={t('sessions.frames.enlargeWithDescription', {
                    time: formatClock(frame.timestampMs),
                    description: reason,
                  })}
                >
                  <FrameImage sessionDir={sessionDir} frame={frame} />
                </button>
                <div className="frame-card__caption">
                  <span className="frame-card__time">{formatClock(frame.timestampMs)}</span>
                </div>
                <p className="brief-frames__reason">{reason}</p>
              </li>
            ))}
          </ul>
        </BriefSection>
      ) : null}

      {brief.quotes.length > 0 ? (
        <BriefSection id="quotes" title={t('sessions.brief.quotes')}>
          <ul className="brief-quotes">
            {brief.quotes.map((quote, index) => {
              const time = formatClock(quote.atMs);
              const speaker =
                quote.speaker === 'me' || quote.speaker === 'others'
                  ? t(`sessions.transcript.speaker.${quote.speaker}`)
                  : quote.speaker;
              return (
                <li key={`${index}-${quote.atMs}`} className="brief-quote">
                  {onSeek ? (
                    <button
                      type="button"
                      className="transcript-line__time brief-quote__time"
                      onClick={() => seek(quote.atMs)}
                      aria-label={t('sessions.transcript.seekTo', { time })}
                    >
                      {time}
                    </button>
                  ) : (
                    <span className="transcript-line__time transcript-line__time--plain brief-quote__time">
                      {time}
                    </span>
                  )}
                  <span className="brief-quote__speaker">{speaker}</span>
                  <p className="brief-quote__text">{quote.text}</p>
                </li>
              );
            })}
          </ul>
        </BriefSection>
      ) : null}

      {brief.suggestedSteps.length > 0 ? (
        <section className="brief__section brief__suggestion" aria-labelledby="brief-steps">
          <h3 id="brief-steps" className="brief__heading">
            {t('sessions.brief.suggestedSteps')}
            <span className="brief-badge">{t('sessions.brief.suggestionBadge')}</span>
          </h3>
          <p className="field__hint">{t('sessions.brief.suggestionHint')}</p>
          <ul className="brief__list">
            {brief.suggestedSteps.map((step, index) => (
              <li key={`${index}-${step}`}>{step}</li>
            ))}
          </ul>
        </section>
      ) : null}
    </>
  );
}

function BriefSection({ id, title, children }: { id: string; title: string; children: ReactNode }) {
  return (
    <section className="brief__section" aria-labelledby={`brief-${id}`}>
      <h3 id={`brief-${id}`} className="brief__heading">
        {title}
      </h3>
      {children}
    </section>
  );
}

function BriefList({ id, title, items }: { id: string; title: string; items: readonly string[] }) {
  if (items.length === 0) return null;
  return (
    <BriefSection id={id} title={title}>
      <ul className="brief__list">
        {items.map((entry, index) => (
          <li key={`${index}-${entry}`}>{entry}</li>
        ))}
      </ul>
    </BriefSection>
  );
}

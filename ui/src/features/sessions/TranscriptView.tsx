import { memo, useCallback, useEffect, useRef, useState } from 'react';
import type { TFunction } from 'i18next';
import { useTranslation } from 'react-i18next';

import { Button } from '../../components/ui/Button';
import type { Frame, TranscriptLine } from './api';
import { formatClock } from './format';
import { nearestFrame } from './frames';
import { FrameImage } from './FrameImage';
import { FrameViewer } from './FrameViewer';

import './sessions.css';

interface RowProps {
  line: TranscriptLine;
  index: number;
  speaker: string;
  /** Which side spoke: it picks the speaker's colour. Any other label has none. */
  side: 'me' | 'others' | 'other';
  selected: boolean;
  /** Without frames the timestamp is plain text: there is nothing to select. */
  selectable: boolean;
  onSelect: (index: number) => void;
}

const TranscriptRow = memo(function TranscriptRow({
  line,
  index,
  speaker,
  side,
  selected,
  selectable,
  onSelect,
}: RowProps) {
  const { t } = useTranslation();
  const time = formatClock(line.startMs);
  return (
    <li
      className={`transcript-line transcript-line--${side}${selected ? ' transcript-line--selected' : ''}`}
      onClick={() => {
        // Dragging to copy some text also ends in a click; that must not move the selection.
        if (selectable && !window.getSelection()?.toString()) onSelect(index);
      }}
    >
      {selectable ? (
        <button
          type="button"
          className="transcript-line__time"
          aria-pressed={selected}
          aria-label={t('sessions.transcript.showFrame', { time })}
          onClick={(event) => {
            event.stopPropagation();
            onSelect(index);
          }}
        >
          {time}
        </button>
      ) : (
        <span className="transcript-line__time transcript-line__time--plain">{time}</span>
      )}
      <span className="transcript-line__speaker">{speaker}</span>
      <span className="transcript-line__text">{line.text}</span>
    </li>
  );
});

/** `me` and `others` are localised; any other label is shown as the transcript has it. */
function speakerOf(
  line: TranscriptLine,
  t: TFunction,
): { label: string; side: 'me' | 'others' | 'other' } {
  const speaker = line.speaker ?? (line.track === 'system' ? 'others' : 'me');
  if (speaker === 'me' || speaker === 'others') {
    return { label: t(`sessions.transcript.speaker.${speaker}`), side: speaker };
  }
  return { label: speaker, side: 'other' };
}

/**
 * The transcript with a time stamp and speaker per line. Choosing a line shows the screen frame
 * closest in time to it.
 */
export function TranscriptView({
  sessionDir,
  lines,
  frames,
}: {
  sessionDir: string;
  lines: readonly TranscriptLine[];
  frames: readonly Frame[];
}) {
  const { t } = useTranslation();
  const [selectedIndex, setSelectedIndex] = useState<number | null>(null);
  const [viewing, setViewing] = useState<number | null>(null);
  const select = useCallback((index: number) => setSelectedIndex(index), []);
  const linesRef = useRef<HTMLOListElement>(null);
  const [scrollable, setScrollable] = useState(false);

  // The box is a keyboard stop only while it really scrolls (a short transcript has nothing to scroll).
  useEffect(() => {
    const element = linesRef.current;
    if (!element) return;
    const measure = () => setScrollable(element.scrollHeight > element.clientHeight);
    measure();
    if (typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [lines.length]);

  if (lines.length === 0) {
    return <p className="note__none">{t('sessions.transcript.empty')}</p>;
  }

  const selectedLine = selectedIndex === null ? undefined : lines[selectedIndex];
  const frame = selectedLine ? nearestFrame(frames, selectedLine.startMs) : null;
  const frameIndex = frame ? frames.indexOf(frame) : -1;
  const hasFrames = frames.length > 0;

  return (
    <div className="transcript">
      {hasFrames ? (
        <aside className="transcript__preview" aria-label={t('sessions.transcript.previewLabel')}>
          {selectedLine && frame ? (
            <>
              <FrameImage sessionDir={sessionDir} frame={frame} />
              <div className="transcript__preview-caption" role="status">
                <p>
                  {t('sessions.transcript.nearestFrame', {
                    time: formatClock(selectedLine.startMs),
                    frameTime: formatClock(frame.timestampMs),
                  })}
                </p>
                {frame.description ? (
                  <p className="transcript__preview-description">{frame.description}</p>
                ) : null}
                <Button size="sm" onClick={() => setViewing(frameIndex)}>
                  {t('sessions.frames.enlargeShort')}
                </Button>
              </div>
            </>
          ) : (
            <p className="transcript__preview-hint">{t('sessions.transcript.previewHint')}</p>
          )}
        </aside>
      ) : null}

      {/* A long meeting scrolls inside its own box; once it scrolls it is a keyboard stop so the keys can scroll it. */}
      <ol
        ref={linesRef}
        className="transcript__lines"
        aria-label={t('sessions.transcript.listLabel')}
        tabIndex={scrollable ? 0 : undefined}
      >
        {lines.map((line, index) => {
          const { label, side } = speakerOf(line, t);
          return (
            <TranscriptRow
              key={`${line.track}-${line.startMs}-${index}`}
              line={line}
              index={index}
              speaker={label}
              side={side}
              selected={index === selectedIndex}
              selectable={hasFrames}
              onSelect={select}
            />
          );
        })}
      </ol>

      <FrameViewer
        sessionDir={sessionDir}
        frames={frames}
        index={viewing}
        onIndexChange={setViewing}
        onClose={() => setViewing(null)}
      />
    </div>
  );
}

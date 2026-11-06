import { memo, useCallback, useState } from 'react';
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
  selected: boolean;
  /** Without frames the timestamp is plain text: there is nothing to select. */
  selectable: boolean;
  onSelect: (index: number) => void;
}

const TranscriptRow = memo(function TranscriptRow({
  line,
  index,
  speaker,
  selected,
  selectable,
  onSelect,
}: RowProps) {
  const { t } = useTranslation();
  const time = formatClock(line.startMs);
  return (
    <li
      className={selected ? 'transcript-line transcript-line--selected' : 'transcript-line'}
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
function speakerLabel(line: TranscriptLine, t: TFunction): string {
  const speaker = line.speaker ?? (line.track === 'system' ? 'others' : 'me');
  if (speaker === 'me' || speaker === 'others') return t(`sessions.transcript.speaker.${speaker}`);
  return speaker;
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

  if (lines.length === 0) {
    return <p className="library__empty">{t('sessions.transcript.empty')}</p>;
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

      <ol className="transcript__lines" aria-label={t('sessions.transcript.listLabel')}>
        {lines.map((line, index) => (
          <TranscriptRow
            key={`${line.track}-${line.startMs}-${index}`}
            line={line}
            index={index}
            speaker={speakerLabel(line, t)}
            selected={index === selectedIndex}
            selectable={hasFrames}
            onSelect={select}
          />
        ))}
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

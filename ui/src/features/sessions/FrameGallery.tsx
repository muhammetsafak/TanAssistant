import { useState } from 'react';
import { useTranslation } from 'react-i18next';

import type { Frame } from './api';
import { formatClock } from './format';
import { FrameImage } from './FrameImage';
import { FrameViewer } from './FrameViewer';

import './sessions.css';

/** Every stored frame as a thumbnail with its time and description; a click enlarges it. */
export function FrameGallery({
  sessionDir,
  frames,
  keyFrameIds = [],
}: {
  sessionDir: string;
  frames: readonly Frame[];
  /** Frames the note refers to; they are marked, since the export carries only those. */
  keyFrameIds?: readonly string[];
}) {
  const { t } = useTranslation();
  const [viewing, setViewing] = useState<number | null>(null);

  if (frames.length === 0) {
    return <p className="note__none">{t('sessions.frames.empty')}</p>;
  }

  return (
    <>
      <ul className="frame-grid" aria-label={t('sessions.frames.galleryLabel')}>
        {frames.map((frame, index) => (
          <li key={frame.id} className="frame-card">
            <button
              type="button"
              className="frame-card__open"
              onClick={() => setViewing(index)}
              title={frame.description ?? undefined}
              aria-label={
                frame.description
                  ? t('sessions.frames.enlargeWithDescription', {
                      time: formatClock(frame.timestampMs),
                      description: frame.description,
                    })
                  : t('sessions.frames.enlarge', { time: formatClock(frame.timestampMs) })
              }
            >
              <FrameImage sessionDir={sessionDir} frame={frame} />
            </button>
            <div className="frame-card__caption">
              <span className="frame-card__time">{formatClock(frame.timestampMs)}</span>
              {keyFrameIds.includes(frame.id) ? (
                <span className="frame-card__key">{t('sessions.frames.inNote')}</span>
              ) : null}
            </div>
          </li>
        ))}
      </ul>
      <FrameViewer
        sessionDir={sessionDir}
        frames={frames}
        index={viewing}
        onIndexChange={setViewing}
        onClose={() => setViewing(null)}
      />
    </>
  );
}

import { useState } from 'react';
import { useTranslation } from 'react-i18next';

import { frameImageUrl, type Frame } from './api';
import { formatClock } from './format';

import './sessions.css';

/**
 * A frame image at a fixed 16:9 box (spans only: it also sits inside buttons), so a slow or missing image never moves the layout. The image
 * comes from the application data directory only (see `frameImageUrl`); when it cannot be shown
 * the box says so instead of leaving a broken-image icon.
 */
export function FrameImage({ sessionDir, frame }: { sessionDir: string; frame: Frame }) {
  const src = frameImageUrl(sessionDir, frame.path);
  // A different frame is a different image: the failure flag must not carry over.
  return <FrameImageBox key={`${src ?? ''}|${frame.id}`} src={src} frame={frame} />;
}

function FrameImageBox({ src, frame }: { src: string | null; frame: Frame }) {
  const { t } = useTranslation();
  const [failed, setFailed] = useState(false);
  const time = formatClock(frame.timestampMs);

  return (
    <span className="frame-image">
      {src && !failed ? (
        <img
          src={src}
          alt={frame.description ?? t('sessions.frames.altAt', { time })}
          loading="lazy"
          decoding="async"
          onError={() => setFailed(true)}
        />
      ) : (
        <span
          className="frame-image__missing"
          role="img"
          aria-label={t('sessions.frames.unavailableAt', { time })}
        >
          {t('sessions.frames.unavailable')}
        </span>
      )}
    </span>
  );
}

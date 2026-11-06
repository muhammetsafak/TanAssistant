import type { KeyboardEvent } from 'react';
import { useTranslation } from 'react-i18next';

import { Button } from '../../components/ui/Button';
import { Dialog } from '../../components/ui/Dialog';
import type { Frame } from './api';
import { formatClock } from './format';
import { FrameImage } from './FrameImage';

import './sessions.css';

export interface FrameViewerProps {
  sessionDir: string;
  frames: readonly Frame[];
  /** Index into `frames` of the frame shown; `null` closes the viewer. */
  index: number | null;
  onIndexChange: (index: number) => void;
  onClose: () => void;
}

/** A frame enlarged in a dialog, with its description and previous/next navigation. */
export function FrameViewer({
  sessionDir,
  frames,
  index,
  onIndexChange,
  onClose,
}: FrameViewerProps) {
  const { t } = useTranslation();
  const frame = index === null ? undefined : frames[index];
  if (index === null || !frame) return null;

  const hasPrevious = index > 0;
  const hasNext = index < frames.length - 1;

  function handleKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (event.key === 'ArrowLeft' && hasPrevious && index !== null) {
      event.preventDefault();
      onIndexChange(index - 1);
    } else if (event.key === 'ArrowRight' && hasNext && index !== null) {
      event.preventDefault();
      onIndexChange(index + 1);
    }
  }

  return (
    <Dialog
      open
      onClose={onClose}
      title={t('sessions.frames.viewerTitle', { time: formatClock(frame.timestampMs) })}
      footer={
        <>
          <Button onClick={() => onIndexChange(index - 1)} disabled={!hasPrevious}>
            {t('sessions.frames.previous')}
          </Button>
          <span className="frame-viewer__position" aria-live="polite">
            {t('sessions.frames.position', { current: index + 1, total: frames.length })}
          </span>
          <Button onClick={() => onIndexChange(index + 1)} disabled={!hasNext}>
            {t('sessions.frames.next')}
          </Button>
          <Button variant="primary" data-autofocus onClick={onClose}>
            {t('common.close')}
          </Button>
        </>
      }
    >
      {/* Arrow keys bubble up from the focused button. */}
      <div className="frame-viewer" onKeyDown={handleKeyDown}>
        <FrameImage sessionDir={sessionDir} frame={frame} />
        {frame.description ? (
          <p className="frame-viewer__description">{frame.description}</p>
        ) : (
          <p className="frame-viewer__description frame-viewer__description--none">
            {t('sessions.frames.noDescription')}
          </p>
        )}
      </div>
    </Dialog>
  );
}

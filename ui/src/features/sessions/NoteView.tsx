import { useState } from 'react';
import { useTranslation } from 'react-i18next';

import type { Frame, Note } from './api';
import { formatClock, formatDateTime } from './format';
import { FrameImage } from './FrameImage';
import { FrameViewer } from './FrameViewer';

import './sessions.css';

/** The generated note: summary, decisions, open questions and the key screen frames. */
export function NoteView({
  note,
  sessionDir,
  frames,
}: {
  note: Note;
  sessionDir: string;
  frames: readonly Frame[];
}) {
  const { t, i18n } = useTranslation();
  const [viewing, setViewing] = useState<number | null>(null);

  // The note lists frame ids in order; a frame that no longer exists is simply left out.
  const keyFrames = note.keyFrameIds
    .map((id) => frames.find((frame) => frame.id === id))
    .filter((frame): frame is Frame => frame !== undefined);

  return (
    <div className="note">
      <p className="note__meta">
        {t('sessions.note.language', {
          language: t(`languageNames.${note.language}`, { defaultValue: note.language }),
        })}
        {note.createdAt !== null
          ? ` · ${t('sessions.note.createdAt', { date: formatDateTime(note.createdAt, i18n.language) })}`
          : ''}
      </p>

      <section className="note__section" aria-labelledby="note-summary">
        <h3 id="note-summary">{t('sessions.note.summary')}</h3>
        {note.summary ? (
          <p className="note__summary">{note.summary}</p>
        ) : (
          <p className="note__none">{t('sessions.note.noSummary')}</p>
        )}
      </section>

      <section className="note__section" aria-labelledby="note-decisions">
        <h3 id="note-decisions">{t('sessions.note.decisions')}</h3>
        {note.decisions.length > 0 ? (
          <ul>
            {note.decisions.map((decision, index) => (
              <li key={`${index}-${decision}`}>{decision}</li>
            ))}
          </ul>
        ) : (
          <p className="note__none">{t('sessions.note.noDecisions')}</p>
        )}
      </section>

      <section className="note__section" aria-labelledby="note-questions">
        <h3 id="note-questions">{t('sessions.note.openQuestions')}</h3>
        {note.openQuestions.length > 0 ? (
          <ul>
            {note.openQuestions.map((question, index) => (
              <li key={`${index}-${question}`}>{question}</li>
            ))}
          </ul>
        ) : (
          <p className="note__none">{t('sessions.note.noOpenQuestions')}</p>
        )}
      </section>

      <section className="note__section" aria-labelledby="note-frames">
        <h3 id="note-frames">{t('sessions.note.keyFrames')}</h3>
        {keyFrames.length > 0 ? (
          <ul className="frame-grid">
            {keyFrames.map((frame, index) => (
              <li key={frame.id} className="frame-card">
                <button
                  type="button"
                  className="frame-card__open"
                  onClick={() => setViewing(index)}
                  aria-label={t('sessions.frames.enlarge', {
                    time: formatClock(frame.timestampMs),
                  })}
                >
                  <FrameImage sessionDir={sessionDir} frame={frame} />
                </button>
                <div className="frame-card__caption">
                  <span className="frame-card__time">{formatClock(frame.timestampMs)}</span>
                  {frame.description ? (
                    <span className="frame-card__text">{frame.description}</span>
                  ) : null}
                </div>
              </li>
            ))}
          </ul>
        ) : (
          <p className="note__none">{t('sessions.note.noKeyFrames')}</p>
        )}
      </section>

      <FrameViewer
        sessionDir={sessionDir}
        frames={keyFrames}
        index={viewing}
        onIndexChange={setViewing}
        onClose={() => setViewing(null)}
      />
    </div>
  );
}

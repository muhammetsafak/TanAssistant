import { useTranslation } from 'react-i18next';

import { TanIllustration } from '../../components/ui/TanIllustration';
import { ActionItems } from './ActionItems';
import type { Note } from './api';
import { formatDateTime } from './format';

import './sessions.css';

/** The generated note: summary, decisions, action items and open questions. */
export function NoteView({ note, sessionId }: { note: Note; sessionId: string }) {
  const { t, i18n } = useTranslation();

  return (
    <div className="note">
      <section className="note__section note__section--summary" aria-labelledby="note-summary">
        <div className="note__summary-body">
          <h2 id="note-summary">{t('sessions.note.summary')}</h2>
          {note.summary ? (
            <p className="note__summary">{note.summary}</p>
          ) : (
            <p className="note__none">{t('sessions.note.noSummary')}</p>
          )}
        </div>
        <TanIllustration pose="approving" width={76} decorative className="note__tan" />
      </section>

      <section className="note__section" aria-labelledby="note-decisions">
        <h2 id="note-decisions">{t('sessions.note.decisions')}</h2>
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

      <ActionItems sessionId={sessionId} items={note.actionItems} />

      <section className="note__section" aria-labelledby="note-questions">
        <h2 id="note-questions">{t('sessions.note.openQuestions')}</h2>
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

      <p className="note__meta">
        {t('sessions.note.language', {
          language: t(`languageNames.${note.language}`, { defaultValue: note.language }),
        })}
        {note.createdAt !== null
          ? ` · ${t('sessions.note.createdAt', { date: formatDateTime(note.createdAt, i18n.language) })}`
          : ''}
      </p>
    </div>
  );
}

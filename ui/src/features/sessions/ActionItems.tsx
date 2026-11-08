import { useTranslation } from 'react-i18next';

import { useCommandErrorHandler } from '../../ipc/errors';
import { useToggleActionItem, type ActionItem } from './api';
import { formatUtcDate } from './format';

import './sessions.css';

/** The note's tasks; ticking one saves at once and puts the tick back if the save fails. */
export function ActionItems({
  sessionId,
  items,
}: {
  sessionId: string;
  items: readonly ActionItem[];
}) {
  const { t, i18n } = useTranslation();
  const handleError = useCommandErrorHandler();
  const toggle = useToggleActionItem(sessionId);

  const done = items.filter((item) => item.done).length;

  return (
    <section className="note__section actions" aria-labelledby="note-actions">
      <div className="actions__head">
        <h2 id="note-actions">{t('sessions.actions.title')}</h2>
        {items.length > 0 ? (
          <p className="actions__progress">
            {t('sessions.actions.progress', { done, total: items.length })}
          </p>
        ) : null}
      </div>
      {items.length === 0 ? (
        <p className="note__none">{t('sessions.actions.empty')}</p>
      ) : (
        <ul className="action-list" aria-label={t('sessions.actions.listLabel')}>
          {items.map((item) => (
            <li key={item.id} className="action-item">
              <label className="action-item__label">
                <input
                  type="checkbox"
                  checked={item.done}
                  onChange={(event) =>
                    toggle.mutate(
                      { actionItemId: item.id, done: event.target.checked },
                      { onError: handleError },
                    )
                  }
                />
                <span
                  className={
                    item.done ? 'action-item__task action-item__task--done' : 'action-item__task'
                  }
                >
                  {item.task}
                </span>
              </label>
              {item.owner || item.due !== null ? (
                <p className="action-item__meta">
                  {item.owner ? (
                    <span>{t('sessions.actions.owner', { owner: item.owner })}</span>
                  ) : null}
                  {item.due !== null ? (
                    <span>
                      {t('sessions.actions.due', { date: formatUtcDate(item.due, i18n.language) })}
                    </span>
                  ) : null}
                </p>
              ) : null}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { Button } from '../../components/ui/Button';
import { useCommandErrorHandler } from '../../ipc/errors';
import {
  usePeople,
  useSessionParticipants,
  useSetActionItemOwner,
  useToggleActionItem,
  type ActionItem,
  type Frame,
} from './api';
import { ActionBriefPanel } from './ActionBriefPanel';
import { formatUtcDate } from './format';
import { OwnerPicker } from './OwnerPicker';

import './sessions.css';

/**
 * The note's tasks. Ticking one, or changing its owner, saves at once and puts the old state back
 * if the save fails. "Detail" opens the task's brief in a drawer.
 */
export function ActionItems({
  sessionId,
  sessionDir,
  frames,
  items,
  onSeek,
}: {
  sessionId: string;
  sessionDir: string;
  frames: readonly Frame[];
  items: readonly ActionItem[];
  /** Given when there is audio to play; a quote of the brief moves the player to its time. */
  onSeek?: (ms: number) => void;
}) {
  const { t, i18n } = useTranslation();
  const handleError = useCommandErrorHandler();
  const toggle = useToggleActionItem(sessionId);
  const setOwner = useSetActionItemOwner(sessionId);
  const people = usePeople();
  const participants = useSessionParticipants(sessionId);

  const participantIds = useMemo(
    () => new Set((participants.data ?? []).map((participant) => participant.person.id)),
    [participants.data],
  );

  const [briefItemId, setBriefItemId] = useState<string | null>(null);
  // Looked up by id so a refetch that changes the task text updates the open panel too.
  const briefItem = items.find((item) => item.id === briefItemId);

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
              <div className="action-item__meta">
                <OwnerPicker
                  task={item.task}
                  ownerPersonId={item.ownerPersonId}
                  ownerName={item.ownerName}
                  ownerIsSelf={item.ownerIsSelf}
                  people={people.data ?? []}
                  participantIds={participantIds}
                  onChange={(owner, person) =>
                    setOwner.mutate(
                      { actionItemId: item.id, owner, person },
                      { onError: handleError },
                    )
                  }
                />
                {item.due !== null ? (
                  <p className="action-item__due">
                    {t('sessions.actions.due', { date: formatUtcDate(item.due, i18n.language) })}
                  </p>
                ) : null}
                <Button
                  size="sm"
                  className="action-item__detail"
                  onClick={() => setBriefItemId(item.id)}
                  aria-label={t('sessions.actions.detailFor', { task: item.task })}
                >
                  {t('sessions.actions.detail')}
                </Button>
              </div>
            </li>
          ))}
        </ul>
      )}
      {briefItem ? (
        <ActionBriefPanel
          key={briefItem.id}
          item={briefItem}
          sessionDir={sessionDir}
          frames={frames}
          onSeek={onSeek}
          onClose={() => setBriefItemId(null)}
        />
      ) : null}
    </section>
  );
}

import { useId, useRef, useState, type KeyboardEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { Link, useNavigate, useParams } from 'react-router';

import { Button } from '../components/ui/Button';
import { ConfirmDialog } from '../components/ui/ConfirmDialog';
import { useToast } from '../components/ui/toast-context';
import { SessionsCommandError, useCommandErrorHandler } from '../ipc/errors';
import { ActionItems } from '../features/sessions/ActionItems';
import { useDeleteSession, useSessionDetail, type SessionDetail } from '../features/sessions/api';
import { ExportButton } from '../features/sessions/ExportButton';
import { formatDateTime, formatDuration } from '../features/sessions/format';
import { FrameGallery } from '../features/sessions/FrameGallery';
import { NoteView } from '../features/sessions/NoteView';
import { ProcessingProgress } from '../features/sessions/ProcessingProgress';
import { StatusBadge } from '../features/sessions/StatusBadge';
import { TranscriptView } from '../features/sessions/TranscriptView';

import '../components/ui/ui.css';
import '../features/sessions/sessions.css';
import '../layout/layout.css';

const TABS = ['note', 'actions', 'transcript', 'frames'] as const;
type Tab = (typeof TABS)[number];

export default function SessionDetailPage() {
  const { t } = useTranslation();
  const { sessionId = '' } = useParams();
  const detail = useSessionDetail(sessionId);

  return (
    <section className="page">
      <Link to="/recordings" className="back-link">
        {t('sessions.detail.back')}
      </Link>
      {detail.isPending ? (
        <p role="status">{t('common.loading')}</p>
      ) : detail.isError ? (
        <div className="alert" role="alert">
          <span>
            {detail.error instanceof SessionsCommandError && detail.error.code === 'not_found'
              ? t('sessions.detail.notFound')
              : t('sessions.detail.loadFailed')}
          </span>
          <Button size="sm" onClick={() => void detail.refetch()}>
            {t('common.retry')}
          </Button>
        </div>
      ) : (
        <SessionDetailView detail={detail.data} />
      )}
    </section>
  );
}

function SessionDetailView({ detail }: { detail: SessionDetail }) {
  const { t, i18n } = useTranslation();
  const toast = useToast();
  const navigate = useNavigate();
  const handleError = useCommandErrorHandler();
  const deleteSession = useDeleteSession();
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const { session, note } = detail;

  const busy = session.status === 'recording' || session.status === 'processing';
  const exportable = note !== null || detail.transcript.length > 0;

  function confirmDelete() {
    deleteSession.mutate(session.id, {
      onSuccess: () => {
        toast.success(t('sessions.detail.deleted'));
        void navigate('/recordings');
      },
      onError: (error) => {
        setConfirmingDelete(false);
        handleError(error);
      },
    });
  }

  return (
    <>
      <header className="detail-header">
        <div className="detail-header__title">
          <h1>{session.title || t('sessions.list.untitled')}</h1>
          <StatusBadge status={session.status} />
        </div>
        <dl className="detail-meta">
          <div>
            <dt>{t('sessions.detail.startedAt')}</dt>
            <dd>{formatDateTime(session.startedAt, i18n.language)}</dd>
          </div>
          {session.durationMs !== null ? (
            <div>
              <dt>{t('sessions.detail.duration')}</dt>
              <dd>{formatDuration(session.durationMs, t)}</dd>
            </div>
          ) : null}
          <div>
            <dt>{t('sessions.detail.speechLanguage')}</dt>
            <dd>{t(`languageNames.${session.language}`, { defaultValue: session.language })}</dd>
          </div>
          <div>
            <dt>{t('sessions.detail.noteLanguage')}</dt>
            <dd>
              {t(`languageNames.${session.noteLanguage}`, { defaultValue: session.noteLanguage })}
            </dd>
          </div>
        </dl>
        <div className="detail-header__actions">
          <ExportButton sessionId={session.id} title={session.title} disabled={!exportable} />
          <Button variant="danger" onClick={() => setConfirmingDelete(true)} disabled={busy}>
            {t('sessions.detail.delete')}
          </Button>
        </div>
        {busy ? <p className="field__hint">{t('sessions.detail.deleteBusy')}</p> : null}
      </header>

      <ProcessingProgress detail={detail} />

      <SessionTabs detail={detail} />

      <ConfirmDialog
        open={confirmingDelete}
        title={t('sessions.detail.deleteTitle')}
        message={t('sessions.detail.deleteMessage', { name: session.title })}
        confirmLabel={t('sessions.detail.delete')}
        pending={deleteSession.isPending}
        onConfirm={confirmDelete}
        onCancel={() => setConfirmingDelete(false)}
      />
    </>
  );
}

function SessionTabs({ detail }: { detail: SessionDetail }) {
  const { t } = useTranslation();
  const baseId = useId();
  const [tab, setTab] = useState<Tab>('note');
  const tabRefs = useRef<Partial<Record<Tab, HTMLButtonElement | null>>>({});
  const { note } = detail;

  const counts: Record<Tab, number | null> = {
    note: null,
    actions: note ? note.actionItems.length : null,
    transcript: detail.transcript.length,
    frames: detail.frames.length,
  };

  function moveTo(next: Tab) {
    setTab(next);
    tabRefs.current[next]?.focus();
  }

  function handleKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    const index = TABS.indexOf(tab);
    let target: Tab | undefined;
    if (event.key === 'ArrowRight') target = TABS[(index + 1) % TABS.length];
    else if (event.key === 'ArrowLeft') target = TABS[(index - 1 + TABS.length) % TABS.length];
    else if (event.key === 'Home') target = TABS[0];
    else if (event.key === 'End') target = TABS[TABS.length - 1];
    if (target) {
      event.preventDefault();
      moveTo(target);
    }
  }

  return (
    <div className="tabs">
      <div
        className="tabs__list"
        role="tablist"
        aria-label={t('sessions.detail.tabs.label')}
        onKeyDown={handleKeyDown}
      >
        {TABS.map((entry) => {
          const count = counts[entry];
          return (
            <button
              key={entry}
              ref={(element) => {
                tabRefs.current[entry] = element;
              }}
              type="button"
              role="tab"
              id={`${baseId}-tab-${entry}`}
              aria-selected={tab === entry}
              aria-controls={tab === entry ? `${baseId}-panel-${entry}` : undefined}
              tabIndex={tab === entry ? 0 : -1}
              className="tabs__tab"
              onClick={() => setTab(entry)}
            >
              {t(`sessions.detail.tabs.${entry}`)}
              {count !== null ? <span className="tabs__count">{count}</span> : null}
            </button>
          );
        })}
      </div>
      <div
        className="tabs__panel"
        role="tabpanel"
        id={`${baseId}-panel-${tab}`}
        aria-labelledby={`${baseId}-tab-${tab}`}
        tabIndex={0}
      >
        {tab === 'note' ? (
          note ? (
            <NoteView note={note} sessionDir={detail.sessionDir} frames={detail.frames} />
          ) : (
            <p className="library__empty">{t('sessions.note.none')}</p>
          )
        ) : tab === 'actions' ? (
          note ? (
            <ActionItems sessionId={detail.session.id} items={note.actionItems} />
          ) : (
            <p className="library__empty">{t('sessions.note.none')}</p>
          )
        ) : tab === 'transcript' ? (
          <TranscriptView
            sessionDir={detail.sessionDir}
            lines={detail.transcript}
            frames={detail.frames}
          />
        ) : (
          <FrameGallery sessionDir={detail.sessionDir} frames={detail.frames} />
        )}
      </div>
    </div>
  );
}

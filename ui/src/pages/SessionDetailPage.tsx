import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Link, useNavigate, useParams } from 'react-router';

import { Button } from '../components/ui/Button';
import { ConfirmDialog } from '../components/ui/ConfirmDialog';
import { TanIllustration } from '../components/ui/TanIllustration';
import { useToast } from '../components/ui/toast-context';
import { SessionsCommandError, useCommandErrorHandler } from '../ipc/errors';
import { useDeleteSession, useSessionDetail, type SessionDetail } from '../features/sessions/api';
import { ExportButton } from '../features/sessions/ExportButton';
import { formatDateTime, formatDuration } from '../features/sessions/format';
import { FrameGallery } from '../features/sessions/FrameGallery';
import { NoteView } from '../features/sessions/NoteView';
import { ProcessingProgress, ReprocessButton } from '../features/sessions/ProcessingProgress';
import { StatusBadge } from '../features/sessions/StatusBadge';
import { TranscriptView } from '../features/sessions/TranscriptView';

import '../components/ui/ui.css';
import '../features/sessions/sessions.css';
import '../layout/layout.css';

export default function SessionDetailPage() {
  const { t } = useTranslation();
  const { sessionId = '' } = useParams();
  const detail = useSessionDetail(sessionId);

  return (
    <section className="page session-page">
      <Link to="/recordings" className="back-link">
        {t('sessions.detail.back')}
      </Link>
      {detail.isPending ? (
        <p role="status">{t('common.loading')}</p>
      ) : detail.isError ? (
        <div className="alert alert--with-tan" role="alert">
          <TanIllustration pose="thinking" width={56} decorative />
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
        <div className="detail-header__main">
          <div className="detail-header__title">
            <h1>{session.title || t('sessions.list.untitled')}</h1>
            <StatusBadge status={session.status} />
          </div>
          <dl className="detail-meta">
            <div>
              <dt className="sr-only">{t('sessions.detail.startedAt')}</dt>
              <dd>{formatDateTime(session.startedAt, i18n.language)}</dd>
            </div>
            {session.durationMs !== null ? (
              <div>
                <dt className="sr-only">{t('sessions.detail.duration')}</dt>
                <dd>{formatDuration(session.durationMs, t)}</dd>
              </div>
            ) : null}
            <div>
              <dt className="sr-only">{t('sessions.detail.frameCount')}</dt>
              <dd>{t('sessions.detail.frameCountValue', { count: detail.frames.length })}</dd>
            </div>
            <div>
              <dt className="sr-only">{t('sessions.detail.speechLanguage')}</dt>
              <dd>
                {t('sessions.detail.speechLanguageValue', {
                  language: t(`languageNames.${session.language}`, {
                    defaultValue: session.language,
                  }),
                })}
              </dd>
            </div>
            <div>
              <dt className="sr-only">{t('sessions.detail.noteLanguage')}</dt>
              <dd>
                {t('sessions.detail.noteLanguageValue', {
                  language: t(`languageNames.${session.noteLanguage}`, {
                    defaultValue: session.noteLanguage,
                  }),
                })}
              </dd>
            </div>
          </dl>
        </div>
        <div className="detail-header__actions">
          {session.status === 'processed' ? (
            <ReprocessButton sessionId={session.id} fallbackJob={detail.job} />
          ) : null}
          <ExportButton sessionId={session.id} title={session.title} disabled={!exportable} />
          <Button variant="danger" onClick={() => setConfirmingDelete(true)} disabled={busy}>
            {t('sessions.detail.delete')}
          </Button>
        </div>
        {busy ? <p className="field__hint">{t('sessions.detail.deleteBusy')}</p> : null}
      </header>

      <ProcessingProgress detail={detail} />

      <div className="session-body">
        <article className="session-body__main session-card">
          {note ? (
            <NoteView note={note} sessionId={session.id} />
          ) : (
            <p className="note__none">{t('sessions.note.none')}</p>
          )}
        </article>

        <aside className="session-body__side" aria-label={t('sessions.detail.sideLabel')}>
          <section className="session-card" aria-labelledby="frames-heading">
            <div className="session-card__head">
              <h2 id="frames-heading">{t('sessions.detail.frames')}</h2>
              <span className="session-card__count">{detail.frames.length}</span>
            </div>
            <FrameGallery
              sessionDir={detail.sessionDir}
              frames={detail.frames}
              keyFrameIds={note?.keyFrameIds ?? []}
            />
          </section>

          <section className="session-card" aria-labelledby="transcript-heading">
            <div className="session-card__head">
              <h2 id="transcript-heading">{t('sessions.detail.transcript')}</h2>
              <span className="session-card__count">{detail.transcript.length}</span>
            </div>
            <TranscriptView
              sessionDir={detail.sessionDir}
              lines={detail.transcript}
              frames={detail.frames}
            />
          </section>
        </aside>
      </div>

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

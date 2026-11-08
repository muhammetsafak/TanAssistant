import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Link } from 'react-router';

import { Button } from '../../components/ui/Button';
import { TanIllustration } from '../../components/ui/TanIllustration';
import {
  PAGE_SIZE,
  SESSION_STATUSES,
  useSessionList,
  useSessionSearch,
  type Session,
  type SessionStatus,
} from './api';
import { formatDateTime, formatDuration } from './format';
import { LibraryProcessingCard } from './ProcessingProgress';
import { SearchBox } from './SearchBox';
import { StatusBadge } from './StatusBadge';
import { useDebouncedValue } from './useDebouncedValue';

import '../../components/ui/ui.css';
import './sessions.css';

/** How long typing must pause before a search runs. */
export const SEARCH_DEBOUNCE_MS = 300;

function FilterChip({
  pressed,
  onClick,
  children,
}: {
  pressed: boolean;
  onClick: () => void;
  children: string;
}) {
  return (
    <button type="button" className="filter-chip" aria-pressed={pressed} onClick={onClick}>
      {children}
    </button>
  );
}

export function SessionList() {
  const { t } = useTranslation();
  const [text, setText] = useState('');
  const [status, setStatus] = useState<SessionStatus | null>(null);
  const [offset, setOffset] = useState(0);

  const query = useDebouncedValue(text.trim(), SEARCH_DEBOUNCE_MS);
  // While the user is still typing, the list stays in the mode it was in.
  const searching = query !== '';

  return (
    <section className="library" aria-label={t('sessions.list.title')}>
      <div className="library__header">
        <SearchBox value={text} onChange={setText} />
      </div>

      <div className="library__filters" role="group" aria-label={t('sessions.list.statusFilter')}>
        <FilterChip
          pressed={status === null}
          onClick={() => {
            setStatus(null);
            setOffset(0);
          }}
        >
          {t('sessions.list.filterAll')}
        </FilterChip>
        {SESSION_STATUSES.map((entry) => (
          <FilterChip
            key={entry}
            pressed={status === entry}
            onClick={() => {
              setStatus(entry);
              setOffset(0);
            }}
          >
            {t(`sessions.status.${entry}`)}
          </FilterChip>
        ))}
      </div>

      {searching ? (
        <SearchResults query={query} status={status} />
      ) : (
        <PagedList status={status} offset={offset} onOffsetChange={setOffset} />
      )}
    </section>
  );
}

function PagedList({
  status,
  offset,
  onOffsetChange,
}: {
  status: SessionStatus | null;
  offset: number;
  onOffsetChange: (offset: number) => void;
}) {
  const { t } = useTranslation();
  const list = useSessionList({ status, offset, limit: PAGE_SIZE });

  if (list.isPending) return <p role="status">{t('common.loading')}</p>;
  if (list.isError) {
    return (
      <div className="alert" role="alert">
        <span>{t('sessions.list.loadFailed')}</span>
        <Button size="sm" onClick={() => void list.refetch()}>
          {t('common.retry')}
        </Button>
      </div>
    );
  }

  const { items, total } = list.data;
  if (items.length === 0) {
    return (
      <div className="library__empty">
        <TanIllustration pose="waving" width={96} decorative />
        <p>{status === null ? t('sessions.list.empty') : t('sessions.list.emptyFiltered')}</p>
      </div>
    );
  }

  return (
    <>
      {items
        .filter((item) => item.session.status === 'processing')
        .map((item) => (
          <LibraryProcessingCard key={item.session.id} session={item.session} />
        ))}
      <ul className="session-list" aria-label={t('sessions.list.listLabel')}>
        {items.map((item) => (
          <SessionRow
            key={item.session.id}
            session={item.session}
            actions={
              item.hasNote ? { done: item.actionItemsDone, total: item.actionItemsTotal } : null
            }
          />
        ))}
      </ul>
      <nav className="pager" aria-label={t('sessions.list.pagination')}>
        <Button
          size="sm"
          onClick={() => onOffsetChange(Math.max(0, offset - PAGE_SIZE))}
          disabled={offset === 0}
        >
          {t('sessions.list.previous')}
        </Button>
        <span className="pager__range" aria-live="polite">
          {t('sessions.list.range', {
            from: offset + 1,
            to: Math.min(offset + items.length, total),
            total,
          })}
        </span>
        <Button
          size="sm"
          onClick={() => onOffsetChange(offset + PAGE_SIZE)}
          disabled={offset + items.length >= total}
        >
          {t('sessions.list.next')}
        </Button>
      </nav>
    </>
  );
}

function SearchResults({ query, status }: { query: string; status: SessionStatus | null }) {
  const { t } = useTranslation();
  const search = useSessionSearch(query);

  if (search.isPending) return <p role="status">{t('sessions.search.searching')}</p>;
  if (search.isError) {
    return (
      <div className="alert" role="alert">
        <span>{t('sessions.search.failed')}</span>
        <Button size="sm" onClick={() => void search.refetch()}>
          {t('common.retry')}
        </Button>
      </div>
    );
  }

  const results = search.data.filter((entry) => status === null || entry.session.status === status);
  if (results.length === 0) {
    return <p className="library__empty">{t('sessions.search.noResults', { query })}</p>;
  }

  return (
    <ul className="session-list" aria-label={t('sessions.search.resultsLabel')}>
      {results.map((result) => (
        <SessionRow
          key={result.session.id}
          session={result.session}
          actions={null}
          hits={result.hits.slice(0, 2).map((hit) => ({
            source: hit.source,
            excerpt: hit.excerpt,
          }))}
          hitCount={result.hits.length}
        />
      ))}
    </ul>
  );
}

interface RowHit {
  source: 'transcript' | 'note';
  excerpt: string;
}

/** The calendar day and the short month of a moment, for the date block of a row. */
function dateBlock(ms: number | null, language: string): { day: string; month: string } | null {
  if (ms === null) return null;
  return {
    day: new Intl.DateTimeFormat(language, { day: '2-digit' }).format(ms),
    month: new Intl.DateTimeFormat(language, { month: 'short' }).format(ms),
  };
}

function SessionRow({
  session,
  actions,
  hits,
  hitCount,
}: {
  session: Session;
  actions: { done: number; total: number } | null;
  hits?: RowHit[];
  hitCount?: number;
}) {
  const { t, i18n } = useTranslation();
  const date = dateBlock(session.startedAt, i18n.language);
  return (
    <li className="session-row">
      <div className="session-row__date" aria-hidden="true">
        <span className="session-row__day">{date?.day ?? '—'}</span>
        <span className="session-row__month">{date?.month ?? ''}</span>
      </div>
      <div className="session-row__main">
        {/* The link stretches over the whole row, so the row is one big target. */}
        <Link className="session-row__title" to={`/recordings/${session.id}`}>
          {session.title || t('sessions.list.untitled')}
        </Link>
        {actions && actions.total > 0 ? (
          <p className="session-row__summary">{t('sessions.list.actionsProgress', actions)}</p>
        ) : null}
        {hits && hits.length > 0 ? (
          <ul className="session-row__hits">
            {hits.map((hit, index) => (
              <li key={`${hit.source}-${index}`}>
                <span className="session-row__hit-source">
                  {t(`sessions.search.source.${hit.source}`)}
                </span>{' '}
                {hit.excerpt}
              </li>
            ))}
            {hitCount !== undefined && hitCount > hits.length ? (
              <li className="session-row__hit-more">
                {t('sessions.search.moreHits', { count: hitCount - hits.length })}
              </li>
            ) : null}
          </ul>
        ) : null}
      </div>
      <p className="session-row__meta">
        {/* The date block is a picture of this: the full date and time is what is read. */}
        {session.startedAt !== null ? (
          <span>{formatDateTime(session.startedAt, i18n.language)}</span>
        ) : null}
        {session.durationMs !== null ? <span>{formatDuration(session.durationMs, t)}</span> : null}
      </p>
      <StatusBadge status={session.status} />
    </li>
  );
}

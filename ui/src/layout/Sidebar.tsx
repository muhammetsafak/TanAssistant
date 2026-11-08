import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useEffect, useRef, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { NavLink } from 'react-router';

import tanMark from '../assets/brand/tan-mark.svg';
import { useRecordingStatus } from '../features/recording/api';
import { sessionKeys } from '../features/sessions/keys';
import { useStorageUsage } from '../features/settings/api';
import { formatBytes } from '../features/sessions/format';
import { ipc } from '../ipc/client';

import './layout.css';

/** Decorative nav icons: 18 px, drawn in the text colour; the label next to them names the item. */
function NavIcon({ children }: { children: ReactNode }) {
  return (
    <svg
      className="sidebar__icon"
      width="18"
      height="18"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      {children}
    </svg>
  );
}

const NAV_ITEMS = [
  {
    to: '/recordings',
    labelKey: 'nav.recordings',
    icon: (
      <NavIcon>
        <circle cx="12" cy="12" r="9" />
        <circle cx="12" cy="12" r="4" fill="currentColor" />
      </NavIcon>
    ),
  },
  {
    to: '/providers',
    labelKey: 'nav.providers',
    icon: (
      <NavIcon>
        <path d="M12 3l1.8 4.7L18.5 9.5l-4.7 1.8L12 16l-1.8-4.7L5.5 9.5l4.7-1.8L12 3z" />
        <path d="M19 15v4M17 17h4" />
      </NavIcon>
    ),
  },
  {
    to: '/credentials',
    labelKey: 'nav.credentials',
    icon: (
      <NavIcon>
        <circle cx="8" cy="15" r="4" />
        <path d="M11 12l9-9M16 7l3 3" />
      </NavIcon>
    ),
  },
  {
    to: '/settings',
    labelKey: 'nav.settings',
    icon: (
      <NavIcon>
        <circle cx="12" cy="12" r="3" />
        <path d="M12 2v3M12 19v3M2 12h3M19 12h3M4.9 4.9l2.1 2.1M17 17l2.1 2.1M4.9 19.1L7 17M17 7l2.1-2.1" />
      </NavIcon>
    ),
  },
] as const;

/** How much disk Tan's data uses. Nothing is shown until the number is known. */
function DiskUsageCard() {
  const { t, i18n } = useTranslation();
  const usage = useStorageUsage();
  const queryClient = useQueryClient();
  const recordingState = useRecordingStatus().data?.state;
  const previousState = useRef(recordingState);

  // A recording writes audio and frames: when it starts, pauses or stops the number is stale.
  useEffect(() => {
    if (previousState.current !== undefined && previousState.current !== recordingState) {
      void queryClient.invalidateQueries({ queryKey: sessionKeys.storage });
    }
    previousState.current = recordingState;
  }, [recordingState, queryClient]);

  const total = usage.data?.totalBytes ?? null;
  if (total === null) return null;
  return (
    <div className="sidebar__disk" data-testid="sidebar-disk">
      <span className="sidebar__disk-label">{t('sidebar.disk')}</span>
      <span className="sidebar__disk-value">{formatBytes(total, i18n.language)}</span>
    </div>
  );
}

export function Sidebar() {
  const { t } = useTranslation();
  const { data: version } = useQuery({ queryKey: ['app-version'], queryFn: ipc.appVersion });

  return (
    <aside className="sidebar">
      <p className="sidebar__brand">
        <img className="sidebar__logo" src={tanMark} alt="" width="36" height="36" />
        <span>{t('app.name')}</span>
      </p>
      <nav aria-label={t('nav.label')}>
        <ul className="sidebar__nav">
          {NAV_ITEMS.map((item) => (
            <li key={item.to}>
              <NavLink to={item.to} className="sidebar__link">
                {item.icon}
                <span>{t(item.labelKey)}</span>
              </NavLink>
            </li>
          ))}
        </ul>
      </nav>
      <div className="sidebar__footer">
        <DiskUsageCard />
        {version ? (
          <p className="sidebar__version" data-testid="app-version">
            {t('common.version', { version })}
          </p>
        ) : null}
      </div>
    </aside>
  );
}

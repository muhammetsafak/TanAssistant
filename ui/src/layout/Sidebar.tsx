import { useQuery } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import { NavLink } from 'react-router';

import { ipc } from '../ipc/client';

import './layout.css';

const NAV_ITEMS = [
  { to: '/recordings', labelKey: 'nav.recordings' },
  { to: '/providers', labelKey: 'nav.providers' },
  { to: '/credentials', labelKey: 'nav.credentials' },
  { to: '/settings', labelKey: 'nav.settings' },
] as const;

export function Sidebar() {
  const { t } = useTranslation();
  const { data: version } = useQuery({ queryKey: ['app-version'], queryFn: ipc.appVersion });

  return (
    <aside className="sidebar">
      <p className="sidebar__brand">{t('app.name')}</p>
      <nav aria-label={t('nav.label')}>
        <ul className="sidebar__nav">
          {NAV_ITEMS.map((item) => (
            <li key={item.to}>
              <NavLink to={item.to} className="sidebar__link">
                {t(item.labelKey)}
              </NavLink>
            </li>
          ))}
        </ul>
      </nav>
      {version ? (
        <p className="sidebar__version" data-testid="app-version">
          {t('common.version', { version })}
        </p>
      ) : null}
    </aside>
  );
}

import { Outlet } from 'react-router';

import { ConsentNotice } from '../features/consent/ConsentNotice';
import { useSyncUiLanguage } from '../features/settings/useSettings';
import { Sidebar } from './Sidebar';
import { TopBar } from './TopBar';

import './layout.css';

export function AppLayout() {
  useSyncUiLanguage();

  return (
    <div className="app-shell">
      <Sidebar />
      <TopBar />
      <main className="app-main" tabIndex={-1}>
        <Outlet />
      </main>
      <ConsentNotice />
    </div>
  );
}

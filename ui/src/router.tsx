import { Navigate, Route, Routes } from 'react-router';

import CredentialsPage from './features/credentials/CredentialsPage';
import { AppLayout } from './layout/AppLayout';
import ProvidersPage from './pages/ProvidersPage';
import RecordingsPage from './pages/RecordingsPage';
import SettingsPage from './pages/SettingsPage';

export function AppRoutes() {
  return (
    <Routes>
      <Route element={<AppLayout />}>
        <Route index element={<Navigate to="/recordings" replace />} />
        <Route path="recordings" element={<RecordingsPage />} />
        <Route path="providers" element={<ProvidersPage />} />
        <Route path="credentials" element={<CredentialsPage />} />
        <Route path="settings" element={<SettingsPage />} />
        <Route path="*" element={<Navigate to="/recordings" replace />} />
      </Route>
    </Routes>
  );
}

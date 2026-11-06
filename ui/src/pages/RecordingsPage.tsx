import { useTranslation } from 'react-i18next';

import { RecordingControl } from '../features/recording/RecordingControl';
import { SessionList } from '../features/sessions/SessionList';

import '../layout/layout.css';

export default function RecordingsPage() {
  const { t } = useTranslation();
  return (
    <section className="page">
      <h1>{t('pages.recordings.title')}</h1>
      <RecordingControl />
      <SessionList />
    </section>
  );
}

import { useTranslation } from 'react-i18next';

import { RecordingControl } from '../features/recording/RecordingControl';
import { SessionList } from '../features/sessions/SessionList';

import '../layout/layout.css';

export default function RecordingsPage() {
  const { t } = useTranslation();
  return (
    <section className="page">
      <header className="page__header">
        <h1 className="page__title">{t('pages.recordings.title')}</h1>
        <p className="page__intro">{t('pages.recordings.intro')}</p>
      </header>
      <RecordingControl />
      <SessionList />
    </section>
  );
}

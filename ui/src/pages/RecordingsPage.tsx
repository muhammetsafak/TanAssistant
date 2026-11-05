import { useTranslation } from 'react-i18next';

import '../layout/layout.css';

export default function RecordingsPage() {
  const { t } = useTranslation();
  return (
    <section className="page">
      <h1>{t('pages.recordings.title')}</h1>
      <p className="page__empty">{t('pages.recordings.empty')}</p>
    </section>
  );
}

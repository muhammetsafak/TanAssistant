import { useTranslation } from 'react-i18next';

import '../layout/layout.css';

export default function SettingsPage() {
  const { t } = useTranslation();
  return (
    <section className="page">
      <h1>{t('pages.settings.title')}</h1>
      <p className="page__empty">{t('pages.settings.empty')}</p>
    </section>
  );
}

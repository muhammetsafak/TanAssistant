import { useTranslation } from 'react-i18next';

import '../layout/layout.css';

export default function ProvidersPage() {
  const { t } = useTranslation();
  return (
    <section className="page">
      <h1>{t('pages.providers.title')}</h1>
      <p className="page__empty">{t('pages.providers.empty')}</p>
    </section>
  );
}

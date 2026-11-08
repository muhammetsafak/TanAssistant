import { useTranslation } from 'react-i18next';

import { ProfilesList } from '../features/providers/ProfilesList';
import { TaskAssignments } from '../features/providers/TaskAssignments';

import '../layout/layout.css';

export default function ProvidersPage() {
  const { t } = useTranslation();
  return (
    <section className="page">
      <header className="page__header">
        <h1 className="page__title">{t('pages.providers.title')}</h1>
        <p className="page__intro">{t('providers.intro')}</p>
      </header>
      <ProfilesList />
      <TaskAssignments />
    </section>
  );
}

import { useTranslation } from 'react-i18next';

import { CaptureSettings } from '../features/settings/CaptureSettings';
import { LanguageSettings } from '../features/settings/LanguageSettings';
import { StorageSettings } from '../features/settings/StorageSettings';
import { WhisperModels } from '../features/settings/WhisperModels';

import '../layout/layout.css';

export default function SettingsPage() {
  const { t } = useTranslation();
  return (
    <section className="page">
      <header className="page__header">
        <h1 className="page__title">{t('pages.settings.title')}</h1>
        <p className="page__intro">{t('settings.intro')}</p>
      </header>
      <LanguageSettings />
      <CaptureSettings />
      <WhisperModels />
      <StorageSettings />
    </section>
  );
}

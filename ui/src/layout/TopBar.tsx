import { useId, type ChangeEvent } from 'react';
import { useTranslation } from 'react-i18next';

import { useToast } from '../components/ui/toast-context';
import { useSettings, useUpdateSettings } from '../features/settings/useSettings';
import { SUPPORTED_LANGUAGES, type SupportedLanguage } from '../i18n';
import { useTheme } from '../theme/theme-context';
import { THEME_PREFERENCES, type ThemePreference } from '../theme/theme-storage';

import './layout.css';

const THEME_LABEL_KEYS: Record<ThemePreference, string> = {
  system: 'topBar.themeSystem',
  light: 'topBar.themeLight',
  dark: 'topBar.themeDark',
};

function isSupportedLanguage(value: string): value is SupportedLanguage {
  return (SUPPORTED_LANGUAGES as readonly string[]).includes(value);
}

export function TopBar() {
  const { t, i18n } = useTranslation();
  const toast = useToast();
  const { theme, setTheme } = useTheme();
  const { data: settings } = useSettings();
  const updateSettings = useUpdateSettings();
  const languageId = useId();
  const themeId = useId();

  const current = isSupportedLanguage(i18n.language) ? i18n.language : 'tr';

  function changeLanguage(event: ChangeEvent<HTMLSelectElement>) {
    const next = event.target.value;
    if (!isSupportedLanguage(next)) return;
    const previous = current;
    void i18n.changeLanguage(next);
    // Persist; if the backend refuses, fall back to the language that was active.
    updateSettings.mutate(
      { uiLanguage: next },
      {
        onError: (error) => {
          void i18n.changeLanguage(settings?.uiLanguage ?? previous);
          toast.error(error);
        },
      },
    );
  }

  return (
    <header className="topbar">
      <div className="topbar__control">
        <label htmlFor={languageId}>{t('topBar.language')}</label>
        <select id={languageId} className="select" value={current} onChange={changeLanguage}>
          {SUPPORTED_LANGUAGES.map((code) => (
            <option key={code} value={code}>
              {t(`languages.${code}`)}
            </option>
          ))}
        </select>
      </div>
      <div className="topbar__control">
        <label htmlFor={themeId}>{t('topBar.theme')}</label>
        <select
          id={themeId}
          className="select"
          value={theme}
          onChange={(event) => setTheme(event.target.value as ThemePreference)}
        >
          {THEME_PREFERENCES.map((value) => (
            <option key={value} value={value}>
              {t(THEME_LABEL_KEYS[value])}
            </option>
          ))}
        </select>
      </div>
    </header>
  );
}

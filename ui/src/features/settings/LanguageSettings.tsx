import { useTranslation } from 'react-i18next';

import { Button } from '../../components/ui/Button';
import { useToast } from '../../components/ui/toast-context';
import { SUPPORTED_LANGUAGES, type SupportedLanguage } from '../../i18n';
import type { SettingsPatch } from '../../ipc/client';
import { SelectField } from '../providers/SelectField';
import { AUTO_LANGUAGE, languageOptions } from './languages';
import { useSettings, useUpdateSettings } from './useSettings';

import '../../components/ui/ui.css';
import './settings.css';

function isSupportedLanguage(value: string): value is SupportedLanguage {
  return (SUPPORTED_LANGUAGES as readonly string[]).includes(value);
}

/** Interface language, and the default spoken and note languages of new recordings. */
export function LanguageSettings() {
  const { t } = useTranslation();
  const toast = useToast();
  const settings = useSettings();
  const update = useUpdateSettings();

  function save(patch: SettingsPatch) {
    update.mutate(patch, {
      onSuccess: () => toast.success(t('settings.saved')),
      onError: (error) => toast.error(error),
    });
  }

  return (
    <section className="settings-section" aria-labelledby="settings-language-heading">
      <div>
        <h2 className="settings-section__title" id="settings-language-heading">
          {t('settings.language.title')}
        </h2>
        <p className="settings-section__hint">{t('settings.language.hint')}</p>
      </div>

      {settings.isPending ? (
        <p role="status">{t('common.loading')}</p>
      ) : settings.isError ? (
        <div className="alert" role="alert">
          <span>{t('errors.settingsLoad')}</span>
          <Button size="sm" onClick={() => void settings.refetch()}>
            {t('common.retry')}
          </Button>
        </div>
      ) : (
        <div className="settings-grid">
          <SelectField
            label={t('settings.language.ui')}
            value={settings.data.uiLanguage}
            onChange={(event) => {
              const next = event.target.value;
              if (isSupportedLanguage(next)) save({ uiLanguage: next });
            }}
          >
            {SUPPORTED_LANGUAGES.map((code) => (
              <option key={code} value={code}>
                {t(`languages.${code}`)}
              </option>
            ))}
          </SelectField>

          <SelectField
            label={t('settings.language.speech')}
            value={settings.data.defaultSpeechLanguage}
            onChange={(event) => save({ defaultSpeechLanguage: event.target.value })}
            hint={t('settings.language.speechHint')}
          >
            {languageOptions(settings.data.defaultSpeechLanguage, true).map((code) => (
              <option key={code} value={code}>
                {code === AUTO_LANGUAGE
                  ? t('languageNames.auto')
                  : t(`languageNames.${code}`, { defaultValue: code })}
              </option>
            ))}
          </SelectField>

          <SelectField
            label={t('settings.language.note')}
            value={settings.data.defaultNoteLanguage}
            onChange={(event) => save({ defaultNoteLanguage: event.target.value })}
            hint={t('settings.language.noteHint')}
          >
            {languageOptions(settings.data.defaultNoteLanguage, false).map((code) => (
              <option key={code} value={code}>
                {t(`languageNames.${code}`, { defaultValue: code })}
              </option>
            ))}
          </SelectField>
        </div>
      )}
    </section>
  );
}

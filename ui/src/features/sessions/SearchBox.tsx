import { useId } from 'react';
import { useTranslation } from 'react-i18next';

import './sessions.css';

export interface SearchBoxProps {
  value: string;
  onChange: (value: string) => void;
}

/** The search field of the library. Debouncing is the caller's job, so typing never lags. */
export function SearchBox({ value, onChange }: SearchBoxProps) {
  const { t } = useTranslation();
  const id = useId();
  const hintId = `${id}-hint`;
  return (
    <div className="field session-search">
      <label className="field__label" htmlFor={id}>
        {t('sessions.search.label')}
      </label>
      <input
        id={id}
        type="search"
        className="field__input"
        value={value}
        onChange={(event) => onChange(event.target.value)}
        placeholder={t('sessions.search.placeholder')}
        aria-describedby={hintId}
        autoComplete="off"
        spellCheck={false}
      />
      <p className="field__hint" id={hintId}>
        {t('sessions.search.hint')}
      </p>
    </div>
  );
}

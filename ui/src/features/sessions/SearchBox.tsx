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
    <div className="session-search">
      <label className="session-search__field" htmlFor={id}>
        <svg
          className="session-search__icon"
          width="16"
          height="16"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          aria-hidden="true"
          focusable="false"
        >
          <circle cx="11" cy="11" r="7" />
          <path d="M20 20l-4-4" />
        </svg>
        <span className="sr-only">{t('sessions.search.label')}</span>
        <input
          id={id}
          type="search"
          className="session-search__input"
          value={value}
          onChange={(event) => onChange(event.target.value)}
          placeholder={t('sessions.search.placeholder')}
          aria-describedby={hintId}
          autoComplete="off"
          spellCheck={false}
        />
      </label>
      <p className="session-search__hint" id={hintId}>
        {t('sessions.search.hint')}
      </p>
    </div>
  );
}

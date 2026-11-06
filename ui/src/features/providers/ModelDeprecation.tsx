import { useTranslation } from 'react-i18next';

import { useProviderKinds, type ProviderKind, type ProviderKindInfo } from './api';
import { deprecationFor, useDeprecationText, type DeprecationNotice } from './deprecation';

import './providers.css';

function DeprecationMessage({ notice }: { notice: DeprecationNotice }) {
  const { t } = useTranslation();
  const text = useDeprecationText(notice);
  return (
    <p className="deprecation-badge">
      <span className="deprecation-badge__text">{text}</span>
      {notice.replacement ? (
        <span className="deprecation-badge__replacement">
          {t('providers.deprecation.replacement', { model: notice.replacement })}
        </span>
      ) : null}
    </p>
  );
}

/** A badge shown when the profile's model is scheduled for shutdown; nothing otherwise. */
export function DeprecationBadge({ kind, model }: { kind: ProviderKind; model: string }) {
  const kinds = useProviderKinds();
  const notice = deprecationFor(kinds.data, kind, model);
  return notice ? <DeprecationMessage notice={notice} /> : null;
}

/** The same warning inside the profile form, announced when the typed model becomes a closing one. */
export function DeprecationNote({
  kinds,
  kind,
  model,
}: {
  kinds: readonly ProviderKindInfo[];
  kind: ProviderKind;
  model: string;
}) {
  const notice = deprecationFor(kinds, kind, model);
  return <div aria-live="polite">{notice ? <DeprecationMessage notice={notice} /> : null}</div>;
}

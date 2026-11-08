import { useTranslation } from 'react-i18next';

import { Chip } from '../../components/ui/Chip';
import { WarningIcon } from '../../components/ui/Icons';
import type { SessionStatus } from './api';

import '../../components/ui/ui.css';
import './sessions.css';

/** The session status as a labelled chip: the icon and the colour back up the text, never replace it. */
export function StatusBadge({ status, className }: { status: SessionStatus; className?: string }) {
  const { t } = useTranslation();
  const label = t(`sessions.status.${status}`);

  if (status === 'recording') {
    return (
      <Chip kind="live" className={className}>
        {label}
      </Chip>
    );
  }
  if (status === 'processing') {
    return (
      <Chip kind="processing" className={className}>
        {label}
      </Chip>
    );
  }
  if (status === 'processed') {
    return (
      <Chip kind="ready" className={className}>
        {label}
      </Chip>
    );
  }
  // "Not processed yet" and "failed" have no chip kind of their own; they share the chip shape.
  return (
    <span
      className={['chip', 'status-badge', `status-badge--${status}`, className ?? '']
        .filter(Boolean)
        .join(' ')}
    >
      {status === 'failed' ? <WarningIcon className="chip__icon" /> : null}
      {label}
    </span>
  );
}

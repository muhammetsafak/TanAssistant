import type { ReactNode } from 'react';

import { CheckIcon, ClockIcon, DotIcon, PauseIcon } from './Icons';

import './ui.css';

export type ChipKind = 'live' | 'paused' | 'processing' | 'ready';

export interface ChipProps {
  kind: ChipKind;
  children: ReactNode;
  className?: string;
}

/**
 * A status chip (recording, paused, processing, note ready). Each kind has its own decorative icon;
 * the text next to it always says the same thing.
 */
export function Chip({ kind, children, className }: ChipProps) {
  const icon =
    kind === 'live' ? (
      <DotIcon className="chip__icon" />
    ) : kind === 'paused' ? (
      <PauseIcon className="chip__icon" />
    ) : kind === 'ready' ? (
      <CheckIcon className="chip__icon" />
    ) : (
      <ClockIcon className="chip__icon" />
    );
  return (
    <span className={['chip', `chip--${kind}`, className ?? ''].filter(Boolean).join(' ')}>
      {icon}
      {children}
    </span>
  );
}

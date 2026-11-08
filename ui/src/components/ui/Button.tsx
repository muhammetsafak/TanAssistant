import type { ComponentProps } from 'react';

import { WarningIcon } from './Icons';

import './ui.css';

type ButtonBaseProps = ComponentProps<'button'> & {
  /**
   * `live` is the amber primary action of the recording screen, `primary` the purple one,
   * `secondary` is outlined, `ghost` has no border. `danger` is outlined and always shows a
   * warning icon, so it is never told apart by colour alone. Use it for destructive actions only.
   */
  variant?: 'primary' | 'secondary' | 'danger' | 'live' | 'ghost';
  size?: 'md' | 'sm';
};

/** A round, square button holding only an icon must carry an accessible name. */
export type ButtonProps = ButtonBaseProps &
  ({ iconOnly?: false } | { iconOnly: true; 'aria-label': string });

export function Button({
  variant = 'secondary',
  size = 'md',
  iconOnly = false,
  type = 'button',
  className,
  children,
  ...rest
}: ButtonProps) {
  const classes = [
    'btn',
    `btn--${variant}`,
    size === 'sm' ? 'btn--sm' : '',
    iconOnly ? 'btn--icon' : '',
    className ?? '',
  ]
    .filter(Boolean)
    .join(' ');
  return (
    <button type={type} className={classes} {...rest}>
      {variant === 'danger' && !iconOnly ? <WarningIcon className="btn__icon" /> : null}
      {children}
    </button>
  );
}

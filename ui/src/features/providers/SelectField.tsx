import { useId, type SelectHTMLAttributes } from 'react';

import '../../components/ui/ui.css';

export interface SelectFieldProps extends Omit<SelectHTMLAttributes<HTMLSelectElement>, 'id'> {
  label: string;
  hint?: string;
  error?: string | null;
}

/** A labelled native select; label, hint and error are wired to it like `TextField`'s. */
export function SelectField({ label, hint, error, children, ...selectProps }: SelectFieldProps) {
  const id = useId();
  const hintId = `${id}-hint`;
  const errorId = `${id}-error`;
  const describedBy = [hint ? hintId : null, error ? errorId : null].filter(Boolean).join(' ');

  return (
    <div className="field">
      <label className="field__label" htmlFor={id}>
        {label}
      </label>
      <select
        {...selectProps}
        id={id}
        className="field__input"
        aria-invalid={error ? true : undefined}
        aria-describedby={describedBy || undefined}
      >
        {children}
      </select>
      {hint ? (
        <p className="field__hint" id={hintId}>
          {hint}
        </p>
      ) : null}
      {error ? (
        <p className="field__error" id={errorId}>
          {error}
        </p>
      ) : null}
    </div>
  );
}

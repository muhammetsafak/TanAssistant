/* Decorative icons of the recording controls. The button text always says the same thing. */

const COMMON = {
  width: 14,
  height: 14,
  'aria-hidden': true,
  focusable: false,
  className: 'btn__icon',
} as const;

/** Filled disc: start a recording. */
export function RecordIcon() {
  return (
    <svg {...COMMON} viewBox="0 0 14 14">
      <circle cx="7" cy="7" r="6" fill="currentColor" />
    </svg>
  );
}

/** Two bars: pause. */
export function PauseControlIcon() {
  return (
    <svg {...COMMON} viewBox="0 0 16 16">
      <rect x="3" y="2" width="3.5" height="12" rx="1.2" fill="currentColor" />
      <rect x="9.5" y="2" width="3.5" height="12" rx="1.2" fill="currentColor" />
    </svg>
  );
}

/** Triangle: resume. */
export function PlayIcon() {
  return (
    <svg {...COMMON} viewBox="0 0 16 16">
      <path d="M4 2.5 13 8l-9 5.5Z" fill="currentColor" />
    </svg>
  );
}

/** Rounded square: stop. */
export function StopIcon() {
  return (
    <svg {...COMMON} viewBox="0 0 12 12">
      <rect x="1" y="1" width="10" height="10" rx="2" fill="currentColor" />
    </svg>
  );
}

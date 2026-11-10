/* Decorative status icons. They never carry meaning alone: the text next to them says the same. */

const COMMON = {
  width: 16,
  height: 16,
  viewBox: '0 0 16 16',
  'aria-hidden': true,
  focusable: false,
} as const;

/** Triangle with an exclamation mark: errors and destructive actions. */
export function WarningIcon({ className }: { className?: string }) {
  return (
    <svg {...COMMON} className={className}>
      <path
        d="M8 1.8 15 14H1L8 1.8Z"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinejoin="round"
      />
      <path d="M8 6.2v3.6" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
      <circle cx="8" cy="11.9" r="0.9" fill="currentColor" />
    </svg>
  );
}

/** Check mark: success and finished states. */
export function CheckIcon({ className }: { className?: string }) {
  return (
    <svg {...COMMON} className={className}>
      <path
        d="M3 8.5 6.5 12 13 4.5"
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

/** Circled "i": notices that need no action. */
export function InfoIcon({ className }: { className?: string }) {
  return (
    <svg {...COMMON} className={className}>
      <circle cx="8" cy="8" r="6" fill="none" stroke="currentColor" strokeWidth="1.6" />
      <path d="M8 7.2v3.6" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
      <circle cx="8" cy="5.2" r="0.9" fill="currentColor" />
    </svg>
  );
}

/** Two bars: paused. */
export function PauseIcon({ className }: { className?: string }) {
  return (
    <svg {...COMMON} className={className}>
      <rect x="3" y="2" width="3.5" height="12" rx="1.2" fill="currentColor" />
      <rect x="9.5" y="2" width="3.5" height="12" rx="1.2" fill="currentColor" />
    </svg>
  );
}

/** Filled dot: live recording. */
export function DotIcon({ className }: { className?: string }) {
  return (
    <svg {...COMMON} className={className}>
      <circle cx="8" cy="8" r="5" fill="currentColor" />
    </svg>
  );
}

/** Clock: work in progress. */
export function ClockIcon({ className }: { className?: string }) {
  return (
    <svg {...COMMON} className={className}>
      <circle cx="8" cy="8" r="6" fill="none" stroke="currentColor" strokeWidth="1.6" />
      <path
        d="M8 4.5V8l2.4 1.6"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

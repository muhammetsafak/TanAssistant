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

/** Props of the channel icons: the transcript uses them too, without the button spacing. */
interface ChannelIconProps {
  className?: string;
}

const CHANNEL = {
  width: 14,
  height: 14,
  viewBox: '0 0 16 16',
  fill: 'none',
  stroke: 'currentColor',
  strokeWidth: 1.5,
  strokeLinecap: 'round',
  strokeLinejoin: 'round',
  'aria-hidden': true,
  focusable: false,
} as const;

/** Microphone: the channel is open. */
export function MicIcon({ className = 'btn__icon' }: ChannelIconProps) {
  return (
    <svg {...CHANNEL} className={className}>
      <rect x="5.5" y="1.5" width="5" height="8" rx="2.5" />
      <path d="M3 8a5 5 0 0 0 10 0M8 13v1.5" />
    </svg>
  );
}

/** Microphone with a slash: the channel is muted. */
export function MicOffIcon({ className = 'btn__icon' }: ChannelIconProps) {
  return (
    <svg {...CHANNEL} className={className}>
      <rect x="5.5" y="1.5" width="5" height="8" rx="2.5" />
      <path d="M3 8a5 5 0 0 0 10 0M8 13v1.5M2 2l12 12" />
    </svg>
  );
}

/** Speaker: the system audio is open. */
export function SpeakerIcon({ className = 'btn__icon' }: ChannelIconProps) {
  return (
    <svg {...CHANNEL} className={className}>
      <path d="M2 6h2.5L8 3v10L4.5 10H2Z" />
      <path d="M10.5 5.5a3.5 3.5 0 0 1 0 5M12.5 3.5a6.5 6.5 0 0 1 0 9" />
    </svg>
  );
}

/** Speaker with a cross: the system audio is muted. */
export function SpeakerOffIcon({ className = 'btn__icon' }: ChannelIconProps) {
  return (
    <svg {...CHANNEL} className={className}>
      <path d="M2 6h2.5L8 3v10L4.5 10H2Z" />
      <path d="M11 6l3 4M14 6l-3 4" />
    </svg>
  );
}

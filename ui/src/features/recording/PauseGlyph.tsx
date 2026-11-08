import './recording.css';

/** Two bars in the current text colour: marks a paused recording in place of the pulsing dot. */
export function PauseGlyph() {
  return (
    <svg className="pause-glyph" viewBox="0 0 12 12" width="12" height="12" aria-hidden="true">
      <rect x="2" y="1" width="3" height="10" rx="1" />
      <rect x="7" y="1" width="3" height="10" rx="1" />
    </svg>
  );
}

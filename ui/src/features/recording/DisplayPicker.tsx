import { useId, useRef, useState, type KeyboardEvent } from 'react';
import { useTranslation } from 'react-i18next';

import { THUMBNAIL_REFRESH_MS, useDisplayThumbnail, type Display } from './api';

import './recording.css';

export interface DisplayPickerProps {
  displays: readonly Display[];
  /** The chosen display id; `''` is the default screen. */
  value: string;
  onChange: (displayId: string) => void;
  /** The display list is still being read. */
  loading: boolean;
  /** The display list could not be read. */
  failed: boolean;
}

/**
 * Screen choice with a live preview of every screen, so identical monitors can be told apart.
 * The previews are requested only while the list is open.
 */
export function DisplayPicker({ displays, value, onChange, loading, failed }: DisplayPickerProps) {
  const { t } = useTranslation();
  const id = useId();
  const labelId = `${id}-label`;
  const valueId = `${id}-value`;
  const panelId = `${id}-panel`;
  const [open, setOpen] = useState(false);
  const toggleRef = useRef<HTMLButtonElement>(null);

  // Escape closes the open list and hands the focus back to the button that opened it.
  function closeOnEscape(event: KeyboardEvent) {
    if (event.key !== 'Escape' || !open) return;
    event.stopPropagation();
    setOpen(false);
    toggleRef.current?.focus();
  }

  const selected = displays.find((display) => display.id === value);
  const selectedName = selected ? selected.name : t('recording.displayDefault');

  return (
    <div className="field display-picker" onKeyDown={closeOnEscape}>
      <div className="display-picker__head">
        <span className="field__label display-picker__title" id={labelId}>
          {t('recording.display')}
        </span>
        {open ? (
          <span className="display-picker__refresh">
            {t('recording.thumbnail.refresh', { seconds: THUMBNAIL_REFRESH_MS / 1000 })}
          </span>
        ) : null}
      </div>
      <button
        ref={toggleRef}
        type="button"
        className="field__input display-picker__toggle"
        aria-expanded={open}
        aria-controls={panelId}
        aria-labelledby={`${labelId} ${valueId}`}
        aria-busy={loading}
        onClick={() => setOpen((current) => !current)}
      >
        <span id={valueId} className="display-picker__value">
          {selectedName}
        </span>
        <span className="display-picker__chevron" aria-hidden="true">
          {open ? '▴' : '▾'}
        </span>
      </button>
      {failed ? <p className="field__hint">{t('recording.displaysFailed')}</p> : null}

      {open ? (
        <div
          id={panelId}
          role="radiogroup"
          aria-labelledby={labelId}
          className="display-picker__list"
        >
          <DefaultCard checked={value === ''} onSelect={() => onChange('')} groupId={id} />
          {displays.map((display) => (
            <DisplayCard
              key={display.id}
              display={display}
              checked={value === display.id}
              onSelect={() => onChange(display.id)}
              groupId={id}
            />
          ))}
        </div>
      ) : null}
      {open ? <p className="field__hint">{t('recording.thumbnail.privacy')}</p> : null}
    </div>
  );
}

function DefaultCard({
  checked,
  onSelect,
  groupId,
}: {
  checked: boolean;
  onSelect: () => void;
  groupId: string;
}) {
  const { t } = useTranslation();
  const nameId = `${groupId}-default-name`;
  return (
    <label className="display-card">
      <input
        type="radio"
        name={groupId}
        checked={checked}
        onChange={onSelect}
        aria-labelledby={nameId}
      />
      <span className="display-card__body">
        <span className="display-card__thumb display-card__thumb--text">
          {t('recording.thumbnail.defaultHint')}
        </span>
        <span className="display-card__name" id={nameId}>
          {t('recording.displayDefault')}
        </span>
      </span>
    </label>
  );
}

function DisplayCard({
  display,
  checked,
  onSelect,
  groupId,
}: {
  display: Display;
  checked: boolean;
  onSelect: () => void;
  groupId: string;
}) {
  const { t } = useTranslation();
  const thumbnail = useDisplayThumbnail(display.id, true);
  const nameId = `${groupId}-${display.id}-name`;
  const metaId = `${groupId}-${display.id}-meta`;
  const meta = `${display.width}×${display.height}${
    display.isPrimary ? `, ${t('recording.primaryDisplay')}` : ''
  }`;

  const hintId = `${groupId}-${display.id}-hint`;
  const image = thumbnail.data;
  let preview;
  let hint: string | null = null;
  if (image?.available && image.dataUrl) {
    preview = (
      <img
        className="display-card__image"
        src={image.dataUrl}
        alt={display.name}
        width={image.width ?? display.width}
        height={image.height ?? display.height}
        style={{
          aspectRatio: `${image.width ?? display.width} / ${image.height ?? display.height}`,
        }}
      />
    );
  } else if (thumbnail.isError || image) {
    const reason = image?.unavailableReason;
    // A reason this UI has no text for (a newer backend) falls back to the generic one.
    const generic = t('recording.thumbnail.unavailable.generic');
    hint =
      thumbnail.isError || !reason
        ? generic
        : t(`recording.thumbnail.unavailable.${reason}`, { defaultValue: generic });
    preview = (
      <span className="display-card__thumb display-card__thumb--text" id={hintId}>
        {hint}
      </span>
    );
  } else {
    hint = t('recording.thumbnail.loading');
    preview = (
      <>
        <span className="display-card__thumb display-card__skeleton" aria-hidden="true" />
        <span className="sr-only" id={hintId}>
          {hint}
        </span>
      </>
    );
  }
  // The hint is read as part of the description: the preview box itself is not in the label.
  const describedBy = hint ? `${metaId} ${hintId}` : metaId;

  return (
    <label className="display-card">
      <input
        type="radio"
        name={groupId}
        checked={checked}
        onChange={onSelect}
        aria-labelledby={nameId}
        aria-describedby={describedBy}
      />
      <span className="display-card__body">
        {preview}
        <span className="display-card__name" id={nameId}>
          {display.name}
        </span>
        <span className="display-card__meta" id={metaId}>
          {meta}
        </span>
      </span>
    </label>
  );
}

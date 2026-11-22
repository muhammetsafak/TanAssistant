import { useEffect, useId, useState, type KeyboardEvent, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';

import { CheckIcon } from './Icons';

import './ui.css';

export interface ComboboxOption {
  /** Identity of the option; unique across all groups. */
  value: string;
  /** What the option says, and what a typed query is matched against. */
  label: string;
}

export interface ComboboxGroup {
  id: string;
  /** The group's heading. A group without one is only set apart by a rule. */
  label?: string;
  options: readonly ComboboxOption[];
}

export interface ComboboxProps {
  /** The field's label (it may carry visually hidden words that tell similar fields apart). */
  label: ReactNode;
  /** Keeps the label for assistive technology only. */
  hideLabel?: boolean;
  /** Every option; the component filters them by what is typed. */
  groups: readonly ComboboxGroup[];
  /** The chosen option, if any: it gets a tick and `aria-selected`. */
  selectedValue: string | null;
  /** What the closed field shows for the current choice. */
  selectedLabel: string;
  placeholder?: string;
  disabled?: boolean;
  maxLength?: number;
  /** Folds text before it is compared (case, diacritics...). Defaults to plain lower case. */
  normalize?: (text: string) => string;
  /**
   * The key two spellings share when they are the same entry; the "add" row is hidden only when
   * the typed text has the key of an existing option. Defaults to `normalize`; pass a stricter
   * one when the search folds more than the data's own notion of "same" (e.g. accents).
   */
  identityKey?: (text: string) => string;
  /**
   * Turns on the "add what was typed" row, shown while the typed text is not exactly the label of
   * an option. Takes the typed text and says what the row reads.
   */
  createLabel?: (text: string) => string;
  onSelect: (option: ComboboxOption) => void;
  /** Called with the trimmed typed text when the "add" row is chosen. */
  onCreate?: (text: string) => void;
  className?: string;
}

type OptionItem = { kind: 'option'; id: string; option: ComboboxOption };
type CreateItem = { kind: 'create'; id: string; text: string };
type Item = OptionItem | CreateItem;

const defaultNormalize = (text: string) => text.toLocaleLowerCase().trim();

/**
 * A searchable single choice: ARIA 1.2 combobox with a listbox pop-up (list autocomplete).
 * Up/Down move through the options, Enter picks the highlighted one, Escape closes the list.
 * Options can be grouped, and an "add what was typed" row lets the user create a new entry.
 */
export function Combobox({
  label,
  hideLabel = false,
  groups,
  selectedValue,
  selectedLabel,
  placeholder,
  disabled = false,
  maxLength,
  normalize = defaultNormalize,
  identityKey = normalize,
  createLabel,
  onSelect,
  onCreate,
  className,
}: ComboboxProps) {
  const { t } = useTranslation();
  const baseId = useId();
  const inputId = `${baseId}-input`;
  const listId = `${baseId}-list`;
  const labelId = `${baseId}-label`;

  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [activeIndex, setActiveIndex] = useState(0);

  const needle = normalize(query);
  const typed = query.trim();
  const typedKey = identityKey(query);

  // The options that match, in display order; `index` is the position in the keyboard order.
  const items: Item[] = [];
  const visibleGroups: {
    group: ComboboxGroup;
    entries: { item: OptionItem; index: number }[];
  }[] = [];
  let exactMatch = false;
  for (const group of groups) {
    const entries: { item: OptionItem; index: number }[] = [];
    for (const option of group.options) {
      const folded = normalize(option.label);
      if (identityKey(option.label) === typedKey) exactMatch = true;
      if (!folded.includes(needle)) continue;
      const item: OptionItem = { kind: 'option', id: `${baseId}-opt-${items.length}`, option };
      entries.push({ item, index: items.length });
      items.push(item);
    }
    if (entries.length > 0) visibleGroups.push({ group, entries });
  }
  const canCreate = Boolean(createLabel && onCreate) && typed !== '' && !exactMatch;
  const createItem: CreateItem | null = canCreate
    ? { kind: 'create', id: `${baseId}-opt-${items.length}`, text: typed }
    : null;
  if (createItem) items.push(createItem);

  const active = items.length === 0 ? -1 : Math.min(activeIndex, items.length - 1);
  const activeId = open && active >= 0 ? items[active]?.id : undefined;

  useEffect(() => {
    if (activeId) document.getElementById(activeId)?.scrollIntoView?.({ block: 'nearest' });
  }, [activeId]);

  const openList = () => {
    if (open || disabled) return;
    const selectedIndex = items.findIndex(
      (item) => item.kind === 'option' && item.option.value === selectedValue,
    );
    setActiveIndex(selectedIndex >= 0 ? selectedIndex : 0);
    setOpen(true);
  };

  const closeList = () => {
    setOpen(false);
    setQuery('');
  };

  const choose = (item: Item) => {
    if (item.kind === 'create') onCreate?.(item.text);
    else onSelect(item.option);
    closeList();
  };

  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      if (!open) {
        openList();
      } else if (items.length > 0) {
        const step = event.key === 'ArrowDown' ? 1 : -1;
        setActiveIndex((active + step + items.length) % items.length);
      }
    } else if (event.key === 'Enter') {
      const current = open ? items[active] : undefined;
      if (current) {
        event.preventDefault();
        choose(current);
      }
    } else if (event.key === 'Escape' && open) {
      // Closes the list only; a dialog around the field stays open.
      event.preventDefault();
      event.stopPropagation();
      closeList();
    } else if (
      !open &&
      !event.ctrlKey &&
      !event.metaKey &&
      !event.altKey &&
      (event.key.length === 1 || event.key === 'Backspace' || event.key === 'Delete')
    ) {
      // Typing over a closed field starts a search instead of editing the shown choice.
      openList();
    }
  };

  const showEmpty = open && items.length === 0;

  return (
    <div className={['field', 'combobox', className ?? ''].filter(Boolean).join(' ')}>
      <label id={labelId} className={hideLabel ? 'sr-only' : 'field__label'} htmlFor={inputId}>
        {label}
      </label>
      <div className="combobox__control">
        <input
          id={inputId}
          className="field__input combobox__input"
          type="text"
          role="combobox"
          autoComplete="off"
          spellCheck={false}
          aria-expanded={open}
          aria-controls={open && items.length > 0 ? listId : undefined}
          aria-autocomplete="list"
          aria-activedescendant={activeId}
          disabled={disabled}
          maxLength={maxLength}
          // The current choice stays visible, dimmed, while the user types over it.
          placeholder={open && selectedLabel ? selectedLabel : placeholder}
          value={open ? query : selectedLabel}
          onChange={(event) => {
            setQuery(event.target.value);
            setActiveIndex(0);
            setOpen(true);
          }}
          onClick={openList}
          onBlur={closeList}
          onKeyDown={onKeyDown}
        />
        {open && items.length > 0 ? (
          // A press inside the pop-up must not take the focus off the field.
          <div
            id={listId}
            role="listbox"
            aria-labelledby={labelId}
            className="combobox__popup"
            onMouseDown={(event) => event.preventDefault()}
          >
            {visibleGroups.map(({ group, entries }) => {
              const headingId = `${baseId}-group-${group.id}`;
              return (
                <div
                  key={group.id}
                  className="combobox__group"
                  role={group.label ? 'group' : undefined}
                  aria-labelledby={group.label ? headingId : undefined}
                >
                  {group.label ? (
                    <div id={headingId} className="combobox__heading" role="presentation">
                      {group.label}
                    </div>
                  ) : null}
                  {entries.map(({ item, index }) => {
                    const { option } = item;
                    const selected = option.value === selectedValue;
                    return (
                      <div
                        key={option.value}
                        id={item.id}
                        role="option"
                        aria-selected={selected}
                        className={
                          index === active
                            ? 'combobox__option combobox__option--active'
                            : 'combobox__option'
                        }
                        onClick={() => choose(item)}
                        onMouseMove={() => setActiveIndex(index)}
                      >
                        <span className="combobox__option-text">{option.label}</span>
                        {selected ? <CheckIcon className="combobox__tick" /> : null}
                      </div>
                    );
                  })}
                </div>
              );
            })}
            {createItem ? (
              <div className="combobox__group">
                <div
                  id={createItem.id}
                  role="option"
                  aria-selected={false}
                  className={
                    active === items.length - 1
                      ? 'combobox__option combobox__option--create combobox__option--active'
                      : 'combobox__option combobox__option--create'
                  }
                  onClick={() => choose(createItem)}
                  onMouseMove={() => setActiveIndex(items.length - 1)}
                >
                  <span className="combobox__option-text">{createLabel?.(createItem.text)}</span>
                </div>
              </div>
            ) : null}
          </div>
        ) : null}
        {showEmpty ? (
          <p className="combobox__popup combobox__empty">{t('combobox.empty')}</p>
        ) : null}
      </div>
      {open && query !== '' ? (
        <p className="sr-only" role="status">
          {t('combobox.results', { count: items.length })}
        </p>
      ) : null}
    </div>
  );
}

import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import '../../i18n';
import { Combobox, type ComboboxGroup, type ComboboxProps } from './Combobox';

const groups: ComboboxGroup[] = [
  { id: 'none', options: [{ value: 'none', label: 'Nobody' }] },
  {
    id: 'team',
    label: 'Team',
    options: [
      { value: 'p1', label: 'Ayşe' },
      { value: 'p2', label: 'Mehmet' },
    ],
  },
  { id: 'rest', label: 'Others', options: [{ value: 'p3', label: 'Çağlar' }] },
];

// Folds case and diacritics the way the app's own search does.
const fold = (text: string) =>
  text.toLocaleLowerCase('tr').normalize('NFD').replace(/[̀-ͯ]/g, '').trim();

function renderCombobox(overrides: Partial<ComboboxProps> = {}) {
  const onSelect = vi.fn();
  const onCreate = vi.fn();
  render(
    <Combobox
      label="Owner"
      groups={groups}
      selectedValue="p2"
      selectedLabel="Mehmet"
      normalize={fold}
      createLabel={(name) => `Add “${name}”`}
      onSelect={onSelect}
      onCreate={onCreate}
      {...overrides}
    />,
  );
  return { onSelect, onCreate, input: screen.getByRole('combobox', { name: 'Owner' }) };
}

describe('Combobox', () => {
  it('shows the current choice while closed and no list', () => {
    const { input } = renderCombobox();

    expect(input).toHaveValue('Mehmet');
    expect(input).toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
  });

  it('opens on click with every option, grouped, and the choice ticked', async () => {
    const user = userEvent.setup();
    const { input } = renderCombobox();

    await user.click(input);

    expect(input).toHaveAttribute('aria-expanded', 'true');
    const list = screen.getByRole('listbox', { name: 'Owner' });
    expect(input).toHaveAttribute('aria-controls', list.id);
    expect(within(list).getAllByRole('option')).toHaveLength(4);
    expect(within(screen.getByRole('group', { name: 'Team' })).getAllByRole('option')).toHaveLength(
      2,
    );
    expect(screen.getByRole('group', { name: 'Others' })).toBeInTheDocument();
    expect(screen.getByRole('option', { name: 'Mehmet' })).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByRole('option', { name: 'Ayşe' })).toHaveAttribute('aria-selected', 'false');
  });

  it('filters while typing, ignoring case and diacritics', async () => {
    const user = userEvent.setup();
    const { input } = renderCombobox();

    await user.click(input);
    await user.type(input, 'AYSE');

    expect(screen.getAllByRole('option').map((option) => option.textContent)).toEqual(['Ayşe']);
    expect(screen.queryByRole('group', { name: 'Others' })).not.toBeInTheDocument();
  });

  it('still offers to add a text that only matches part of a name', async () => {
    const user = userEvent.setup();
    const { input } = renderCombobox();

    await user.click(input);
    await user.type(input, 'ay');

    expect(screen.getAllByRole('option').map((option) => option.textContent)).toEqual([
      'Ayşe',
      'Add “ay”',
    ]);
  });

  it('offers to add the typed text when nothing matches, and passes it trimmed', async () => {
    const user = userEvent.setup();
    const { input, onCreate, onSelect } = renderCombobox();

    await user.click(input);
    await user.type(input, '  Zeynep ');

    const add = screen.getByRole('option', { name: 'Add “Zeynep”' });
    expect(screen.getAllByRole('option')).toHaveLength(1);
    await user.click(add);

    expect(onCreate).toHaveBeenCalledWith('Zeynep');
    expect(onSelect).not.toHaveBeenCalled();
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
  });

  it('does not offer to add what an option already says exactly', async () => {
    const user = userEvent.setup();
    const { input } = renderCombobox();

    await user.click(input);
    await user.type(input, 'mehmet');

    expect(screen.getAllByRole('option')).toHaveLength(1);
    expect(screen.getByRole('option', { name: 'Mehmet' })).toBeInTheDocument();
  });

  it('hides the add row by the identity key, which may be stricter than the search', async () => {
    const user = userEvent.setup();
    const { input } = renderCombobox({
      identityKey: (text) => text.toLocaleLowerCase('tr').trim(),
    });

    await user.click(input);
    await user.type(input, 'ayse');

    // The search finds "Ayşe", but "ayse" is not the same entry as "Ayşe".
    expect(screen.getAllByRole('option').map((option) => option.textContent)).toEqual([
      'Ayşe',
      'Add “ayse”',
    ]);
  });

  it('offers no add row without a way to create', async () => {
    const user = userEvent.setup();
    const { input } = renderCombobox({ createLabel: undefined, onCreate: undefined });

    await user.click(input);
    await user.type(input, 'zzz');

    expect(screen.queryByRole('option')).not.toBeInTheDocument();
    expect(screen.getByText('Eşleşen sonuç yok.')).toBeInTheDocument();
  });

  it('moves with the arrow keys, wraps, and picks with Enter', async () => {
    const user = userEvent.setup();
    const { input, onSelect } = renderCombobox({ selectedValue: null, selectedLabel: '' });

    input.focus();
    await user.keyboard('{ArrowDown}');
    expect(input).toHaveAttribute('aria-expanded', 'true');
    // Nothing was chosen, so the first option is the highlighted one.
    expect(input).toHaveAttribute(
      'aria-activedescendant',
      screen.getByRole('option', { name: 'Nobody' }).id,
    );

    await user.keyboard('{ArrowDown}{ArrowDown}');
    expect(input).toHaveAttribute(
      'aria-activedescendant',
      screen.getByRole('option', { name: 'Mehmet' }).id,
    );

    await user.keyboard('{ArrowUp}{ArrowUp}{ArrowUp}');
    // Up from the first wraps to the last.
    expect(input).toHaveAttribute(
      'aria-activedescendant',
      screen.getByRole('option', { name: 'Çağlar' }).id,
    );

    await user.keyboard('{Enter}');

    expect(onSelect).toHaveBeenCalledWith({ value: 'p3', label: 'Çağlar' });
    expect(input).toHaveAttribute('aria-expanded', 'false');
  });

  it('starts on the chosen option when opened by key', async () => {
    const user = userEvent.setup();
    const { input } = renderCombobox();

    input.focus();
    await user.keyboard('{ArrowDown}');

    expect(input).toHaveAttribute(
      'aria-activedescendant',
      screen.getByRole('option', { name: 'Mehmet' }).id,
    );
  });

  it('starts a search when typing over a closed field, instead of editing the shown choice', async () => {
    const user = userEvent.setup();
    const { input } = renderCombobox();

    input.focus();
    await user.keyboard('ay');

    expect(input).toHaveValue('ay');
    expect(screen.getAllByRole('option').map((option) => option.textContent)).toEqual([
      'Ayşe',
      'Add “ay”',
    ]);
  });

  it('picks the add row with Enter when it is the only result', async () => {
    const user = userEvent.setup();
    const { input, onCreate } = renderCombobox();

    await user.click(input);
    await user.type(input, 'Zeynep{Enter}');

    expect(onCreate).toHaveBeenCalledWith('Zeynep');
  });

  it('closes with Escape without choosing, and keeps the event from a surrounding dialog', async () => {
    const user = userEvent.setup();
    const outer = vi.fn();
    const { onSelect } = renderCombobox();
    document.addEventListener('keydown', outer);

    const input = screen.getByRole('combobox', { name: 'Owner' });
    await user.click(input);
    await user.type(input, 'ay');
    await user.keyboard('{Escape}');

    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
    expect(input).toHaveValue('Mehmet');
    expect(onSelect).not.toHaveBeenCalled();
    // The Escape was consumed by the list; only the earlier keys reached the document.
    expect(outer.mock.calls.some(([event]) => (event as KeyboardEvent).key === 'Escape')).toBe(
      false,
    );
    document.removeEventListener('keydown', outer);
  });

  it('selects an option by click without losing focus', async () => {
    const user = userEvent.setup();
    const { input, onSelect } = renderCombobox();

    await user.click(input);
    await user.click(screen.getByRole('option', { name: 'Ayşe' }));

    expect(onSelect).toHaveBeenCalledWith({ value: 'p1', label: 'Ayşe' });
    expect(input).toHaveFocus();
  });

  it('closes when it loses focus and forgets what was typed', async () => {
    const user = userEvent.setup();
    const { input } = renderCombobox();

    await user.click(input);
    await user.type(input, 'xx');
    await user.tab();

    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
    expect(input).toHaveValue('Mehmet');
  });

  it('does not open when disabled', async () => {
    const user = userEvent.setup();
    const { input } = renderCombobox({ disabled: true });

    await user.click(input);

    expect(input).toBeDisabled();
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
  });
});

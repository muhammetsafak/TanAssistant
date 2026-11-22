import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '../../i18n';
import { ayse, mehmet, people } from './fixtures';
import { OwnerPicker, type OwnerPickerProps } from './OwnerPicker';

function renderPicker(overrides: Partial<OwnerPickerProps> = {}) {
  const onChange = vi.fn();
  render(
    <OwnerPicker
      task="Bütçeyi paylaş"
      ownerPersonId={null}
      ownerName={null}
      ownerIsSelf={false}
      people={people}
      participantIds={new Set(['p2'])}
      onChange={onChange}
      {...overrides}
    />,
  );
  return { onChange, input: screen.getByRole('combobox', { name: 'Sorumlu: Bütçeyi paylaş' }) };
}

describe('OwnerPicker', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
  });

  it('names itself after its task and shows the current owner', () => {
    const { input } = renderPicker({ ownerPersonId: 'p1', ownerName: 'Ayşe' });

    expect(input).toHaveValue('Ayşe');
  });

  it('shows "Ben" for the person who records, whatever name the directory holds', () => {
    const { input } = renderPicker({
      ownerPersonId: 'me',
      ownerName: 'Muhammet',
      ownerIsSelf: true,
    });

    expect(input).toHaveValue('Ben');
  });

  it('lists "no owner", the recorder, the participants and then everyone else', async () => {
    const user = userEvent.setup();
    const { input } = renderPicker();

    await user.click(input);

    expect(screen.getAllByRole('option').map((option) => option.textContent)).toEqual([
      'Sorumlu yok',
      'Ben',
      'Mehmet',
      'Ayşe',
      'Çağlar',
    ]);
    expect(
      within(screen.getByRole('group', { name: 'Katılımcılar' })).getByRole('option'),
    ).toHaveTextContent('Mehmet');
    expect(
      within(screen.getByRole('group', { name: 'Diğer kişiler' })).getAllByRole('option'),
    ).toHaveLength(2);
    // Nobody is the owner yet: "no owner" is the ticked choice.
    expect(screen.getByRole('option', { name: 'Sorumlu yok' })).toHaveAttribute(
      'aria-selected',
      'true',
    );
  });

  it('finds "Ayşe" by typing "ayse"', async () => {
    const user = userEvent.setup();
    const { input, onChange } = renderPicker();

    await user.click(input);
    await user.type(input, 'ayse');

    // "Ayşe" comes first; "ayse" spelled without the accent could also be added as its own name.
    expect(screen.getAllByRole('option').map((option) => option.textContent)).toEqual([
      'Ayşe',
      '“ayse” olarak ekle',
    ]);
    await user.click(screen.getByRole('option', { name: 'Ayşe' }));

    expect(onChange).toHaveBeenCalledWith({ personId: 'p1' }, ayse);
  });

  it('offers to add a name nobody has and reports it as a new name', async () => {
    const user = userEvent.setup();
    const { input, onChange } = renderPicker();

    await user.click(input);
    await user.type(input, 'Zeynep');

    expect(screen.getAllByRole('option')).toHaveLength(1);
    await user.click(screen.getByRole('option', { name: '“Zeynep” olarak ekle' }));

    expect(onChange).toHaveBeenCalledWith({ newName: 'Zeynep' }, null);
  });

  it('still offers to add a name that differs from a listed one only by an accent', async () => {
    const user = userEvent.setup();
    const { input, onChange } = renderPicker({
      people: [...people, { id: 'p4', displayName: 'Alı', isSelf: false }],
    });

    await user.click(input);
    await user.type(input, 'Ali');

    // The search finds "Alı", but the directory treats "Ali" as another person.
    expect(screen.getAllByRole('option').map((option) => option.textContent)).toEqual([
      'Alı',
      '“Ali” olarak ekle',
    ]);
    await user.click(screen.getByRole('option', { name: '“Ali” olarak ekle' }));
    expect(onChange).toHaveBeenCalledWith({ newName: 'Ali' }, null);
  });

  it('picks by keyboard', async () => {
    const user = userEvent.setup();
    const { input, onChange } = renderPicker();

    input.focus();
    // Opens on "no owner" (the chosen one), then Ben, then Mehmet.
    await user.keyboard('{ArrowDown}{ArrowDown}{ArrowDown}{Enter}');

    expect(onChange).toHaveBeenCalledWith({ personId: 'p2' }, mehmet);
  });

  it('clears the owner with "no owner"', async () => {
    const user = userEvent.setup();
    const { input, onChange } = renderPicker({ ownerPersonId: 'p1', ownerName: 'Ayşe' });

    await user.click(input);
    await user.click(screen.getByRole('option', { name: 'Sorumlu yok' }));

    expect(onChange).toHaveBeenCalledWith(null, null);
  });

  it('does not call back for a choice that changes nothing', async () => {
    const user = userEvent.setup();
    const { input, onChange } = renderPicker({ ownerPersonId: 'p1', ownerName: 'Ayşe' });

    await user.click(input);
    await user.click(screen.getByRole('option', { name: 'Ayşe' }));
    await user.click(input);
    await user.type(input, 'ayşe{Enter}');

    expect(onChange).not.toHaveBeenCalled();
  });

  it('does not call back when "no owner" is picked and there is none', async () => {
    const user = userEvent.setup();
    const { input, onChange } = renderPicker();

    await user.click(input);
    await user.click(screen.getByRole('option', { name: 'Sorumlu yok' }));

    expect(onChange).not.toHaveBeenCalled();
  });

  it('speaks English too', async () => {
    await i18n.changeLanguage('en');
    const user = userEvent.setup();
    render(
      <OwnerPicker
        task="Share the budget"
        ownerPersonId={null}
        ownerName={null}
        ownerIsSelf={false}
        people={people}
        participantIds={new Set()}
        onChange={vi.fn()}
      />,
    );

    const input = screen.getByRole('combobox', { name: 'Owner: Share the budget' });
    await user.click(input);
    await user.type(input, 'Zeynep');

    expect(screen.getByRole('option', { name: 'Add “Zeynep”' })).toBeInTheDocument();
    expect(screen.queryByRole('group', { name: 'Participants' })).not.toBeInTheDocument();
  });
});

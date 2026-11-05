import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '../../i18n';
import { commands } from '../../ipc/bindings';
import { ok, renderWithProviders } from '../../test-utils';
import { VariablesTable } from './VariablesTable';

vi.mock('../../ipc/bindings', () => ({
  commands: {
    listVariables: vi.fn(),
    upsertVariable: vi.fn(),
    deleteVariable: vi.fn(),
  },
}));

const variable = { id: 'v1', key: 'DEFAULT_LANG', value: 'tr', description: 'Note language' };

async function renderTable() {
  renderWithProviders(<VariablesTable />);
  await screen.findByText('DEFAULT_LANG');
}

describe('VariablesTable', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
    vi.mocked(commands.listVariables).mockResolvedValue(ok([variable]));
    vi.mocked(commands.upsertVariable).mockResolvedValue(ok(variable));
    vi.mocked(commands.deleteVariable).mockResolvedValue(ok(null));
  });

  afterEach(() => vi.clearAllMocks());

  it('lists key, value and description', async () => {
    await renderTable();

    expect(screen.getByText('tr')).toBeInTheDocument();
    expect(screen.getByText('Note language')).toBeInTheDocument();
  });

  it('shows an empty state', async () => {
    vi.mocked(commands.listVariables).mockResolvedValue(ok([]));
    renderWithProviders(<VariablesTable />);

    expect(await screen.findByText('Henüz variable yok.')).toBeInTheDocument();
  });

  it('adds a variable', async () => {
    const user = userEvent.setup();
    await renderTable();

    await user.click(screen.getByRole('button', { name: 'Variable ekle' }));
    const dialog = screen.getByRole('dialog', { name: 'Variable ekle' });
    await user.type(within(dialog).getByLabelText('Anahtar'), 'REGION');
    await user.type(within(dialog).getByLabelText('Değer'), 'eu');
    await user.click(within(dialog).getByRole('button', { name: 'Kaydet' }));

    await waitFor(() =>
      expect(commands.upsertVariable).toHaveBeenCalledWith({
        id: null,
        key: 'REGION',
        value: 'eu',
        description: '',
      }),
    );
  });

  it('edits a variable with its id', async () => {
    const user = userEvent.setup();
    await renderTable();

    await user.click(screen.getByRole('button', { name: 'Düzenle: DEFAULT_LANG' }));
    const dialog = screen.getByRole('dialog', { name: 'Variable düzenle' });
    const value = within(dialog).getByLabelText('Değer');
    expect(value).toHaveValue('tr');
    await user.clear(value);
    await user.type(value, 'en');
    await user.click(within(dialog).getByRole('button', { name: 'Kaydet' }));

    await waitFor(() =>
      expect(commands.upsertVariable).toHaveBeenCalledWith({
        id: 'v1',
        key: 'DEFAULT_LANG',
        value: 'en',
        description: 'Note language',
      }),
    );
  });

  it('validates required fields before calling the backend', async () => {
    const user = userEvent.setup();
    await renderTable();

    await user.click(screen.getByRole('button', { name: 'Variable ekle' }));
    await user.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'Kaydet' }));

    expect(screen.getByText('Anahtar zorunludur.')).toBeInTheDocument();
    expect(screen.getByText('Değer zorunludur.')).toBeInTheDocument();
    expect(commands.upsertVariable).not.toHaveBeenCalled();
  });

  it('does not delete without confirmation', async () => {
    const user = userEvent.setup();
    await renderTable();

    await user.click(screen.getByRole('button', { name: 'Sil: DEFAULT_LANG' }));
    expect(screen.getByRole('dialog', { name: 'Variable silinsin mi?' })).toBeInTheDocument();
    expect(commands.deleteVariable).not.toHaveBeenCalled();

    await user.keyboard('{Escape}');
    expect(commands.deleteVariable).not.toHaveBeenCalled();
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Sil: DEFAULT_LANG' }));
    await user.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'Sil' }));
    await waitFor(() => expect(commands.deleteVariable).toHaveBeenCalledWith('v1'));
  });
});

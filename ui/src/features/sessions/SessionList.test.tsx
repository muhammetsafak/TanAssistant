import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '../../i18n';
import { commands } from '../../ipc/bindings';
import { fail, ok, renderWithProviders } from '../../test-utils';
import { session } from './fixtures';
import { SessionList } from './SessionList';

vi.mock('../../ipc/bindings', () => ({
  commands: { listSessions: vi.fn(), searchSessions: vi.fn() },
}));

const item = {
  session,
  hasNote: true,
  actionItemsTotal: 4,
  actionItemsDone: 1,
};

describe('SessionList', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
    vi.mocked(commands.listSessions).mockResolvedValue(
      ok({ items: [item], total: 1, offset: 0, limit: 20 }),
    );
    vi.mocked(commands.searchSessions).mockResolvedValue(ok([]));
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it('lists sessions with title, length, status and action progress', async () => {
    renderWithProviders(<SessionList />);

    const row = (await screen.findByRole('link', { name: 'Weekly sync' })).closest('li')!;
    expect(row).toHaveTextContent('45 dk');
    expect(row).toHaveTextContent('Hazır');
    expect(row).toHaveTextContent('1/4 aksiyon');
    expect(screen.getByRole('link', { name: 'Weekly sync' })).toHaveAttribute(
      'href',
      '/recordings/s1',
    );
  });

  it('searches once, after typing pauses, with the whole text', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.searchSessions).mockResolvedValue(
      ok([
        {
          session,
          hits: [{ source: 'transcript', startMs: 5000, excerpt: 'bütçe toplantısı' }],
        },
      ]),
    );
    renderWithProviders(<SessionList />);
    await screen.findByRole('link', { name: 'Weekly sync' });

    await user.type(screen.getByRole('searchbox', { name: 'Ara' }), 'bütçe');

    await waitFor(() => expect(commands.searchSessions).toHaveBeenCalledTimes(1));
    expect(commands.searchSessions).toHaveBeenCalledWith('bütçe');
    const results = await screen.findByRole('list', { name: 'Arama sonuçları' });
    expect(within(results).getByText(/bütçe toplantısı/)).toBeInTheDocument();
    // Settled: no further calls trickle in.
    await new Promise((resolve) => setTimeout(resolve, 450));
    expect(commands.searchSessions).toHaveBeenCalledTimes(1);
  });

  it('filters by status and goes back to page one', async () => {
    const user = userEvent.setup();
    renderWithProviders(<SessionList />);
    await screen.findByRole('link', { name: 'Weekly sync' });

    await user.selectOptions(screen.getByLabelText('Durum'), 'failed');

    await waitFor(() =>
      expect(commands.listSessions).toHaveBeenLastCalledWith(
        { status: 'failed' },
        { offset: 0, limit: 20 },
      ),
    );
  });

  it('pages through long lists', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.listSessions).mockResolvedValue(
      ok({ items: [item], total: 45, offset: 0, limit: 20 }),
    );
    renderWithProviders(<SessionList />);
    await screen.findByRole('link', { name: 'Weekly sync' });
    expect(screen.getByRole('button', { name: 'Önceki' })).toBeDisabled();

    await user.click(screen.getByRole('button', { name: 'Sonraki' }));

    await waitFor(() =>
      expect(commands.listSessions).toHaveBeenLastCalledWith(
        { status: null },
        { offset: 20, limit: 20 },
      ),
    );
  });

  it('shows an empty state and a retry on failure', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.listSessions).mockResolvedValueOnce(fail('internal') as never);
    renderWithProviders(<SessionList />);

    expect(await screen.findByText('Kayıtlar yüklenemedi.')).toBeInTheDocument();
    vi.mocked(commands.listSessions).mockResolvedValue(
      ok({ items: [], total: 0, offset: 0, limit: 20 }),
    );
    await user.click(screen.getByRole('button', { name: 'Yeniden dene' }));

    expect(await screen.findByText(/Henüz kayıt yok/)).toBeInTheDocument();
  });
});

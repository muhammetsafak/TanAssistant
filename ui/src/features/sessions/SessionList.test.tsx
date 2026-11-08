import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '../../i18n';
import { commands } from '../../ipc/bindings';
import { emitEvent } from '../../test-events';
import { fail, ok, renderWithProviders } from '../../test-utils';
import { session } from './fixtures';
import { SessionList } from './SessionList';

vi.mock('../../ipc/bindings', async () => {
  const { stubEvents } = await import('../../test-events');
  return {
    commands: {
      listSessions: vi.fn(),
      searchSessions: vi.fn(),
      processingStatus: vi.fn(),
      cancelProcessing: vi.fn(),
    },
    events: stubEvents(),
  };
});

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

    await user.click(
      within(screen.getByRole('group', { name: 'Durum' })).getByRole('button', {
        name: 'Başarısız',
      }),
    );

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

  it('is a region named after the library, without a second heading of that name', async () => {
    renderWithProviders(<SessionList />);
    await screen.findByRole('link', { name: 'Weekly sync' });

    expect(screen.getByRole('region', { name: 'Kütüphane' })).toBeInTheDocument();
    expect(screen.queryByRole('heading', { name: 'Kütüphane' })).not.toBeInTheDocument();
  });

  it('marks the chosen filter and shows the date block and status chip of a row', async () => {
    const user = userEvent.setup();
    renderWithProviders(<SessionList />);
    const row = (await screen.findByRole('link', { name: 'Weekly sync' })).closest('li')!;

    // The date block is decorative; the full date, year and time are text a screen reader reaches.
    expect(row.querySelector('.session-row__date')).toHaveAttribute('aria-hidden', 'true');
    expect(row.querySelector('.session-row__day')).toHaveTextContent(/^\d{2}$/);
    expect(row.querySelector('.session-row__meta')).toHaveTextContent(/2026/);
    expect(row.querySelector('.session-row__meta')).toHaveTextContent(/\d{2}:\d{2}/);
    expect(row.querySelector('.chip--ready')).toHaveTextContent('Hazır');

    const group = screen.getByRole('group', { name: 'Durum' });
    expect(within(group).getByRole('button', { name: 'Tümü' })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    await user.click(within(group).getByRole('button', { name: 'İşlenmedi' }));
    expect(within(group).getByRole('button', { name: 'İşlenmedi' })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    expect(within(group).getByRole('button', { name: 'Tümü' })).toHaveAttribute(
      'aria-pressed',
      'false',
    );
  });

  it('greets with a waving Tan when the library is empty', async () => {
    vi.mocked(commands.listSessions).mockResolvedValue(
      ok({ items: [], total: 0, offset: 0, limit: 20 }),
    );
    const { container } = renderWithProviders(<SessionList />);

    expect(await screen.findByText(/Henüz kayıt yok/)).toBeInTheDocument();
    expect(container.querySelector('.library__empty img.tan-illustration')).not.toBeNull();
  });

  describe('a session being processed', () => {
    const processing = { ...item, session: { ...session, status: 'processing' as const } };

    beforeEach(() => {
      vi.mocked(commands.listSessions).mockResolvedValue(
        ok({ items: [processing], total: 1, offset: 0, limit: 20 }),
      );
    });

    it('shows a card with progress and cancel while a run is active', async () => {
      const user = userEvent.setup();
      vi.mocked(commands.processingStatus).mockResolvedValue(
        ok({
          running: true,
          job: {
            sessionId: 's1',
            steps: [
              { step: 'transcribe', state: 'running', error: null },
              { step: 'describe_frames', state: 'pending', error: null },
              { step: 'synthesize', state: 'pending', error: null },
            ],
            createdAt: 1,
            updatedAt: 2,
            finishedAt: null,
          },
        }),
      );
      vi.mocked(commands.cancelProcessing).mockResolvedValue(ok(true));
      renderWithProviders(<SessionList />);

      const card = await screen.findByRole('region', { name: 'Weekly sync' });
      expect(
        within(card).getByRole('progressbar', { name: 'İşleme ilerlemesi' }),
      ).toBeInTheDocument();
      expect(
        within(card).getByText('Transkripsiyon', { selector: '.step-list__name' }),
      ).toBeVisible();

      emitEvent('processingProgressEvent', {
        sessionId: 's1',
        step: 'transcribe',
        stepPercent: 50,
        percent: 64,
      });
      expect(await within(card).findByText(/%64/)).toBeInTheDocument();

      await user.click(within(card).getByRole('button', { name: 'İptal et' }));
      expect(commands.cancelProcessing).toHaveBeenCalledWith('s1');
    });

    it('shows no card when nothing is running (a run cut short by closing the app)', async () => {
      vi.mocked(commands.processingStatus).mockResolvedValue(ok({ running: false, job: null }));
      renderWithProviders(<SessionList />);

      await screen.findByRole('link', { name: 'Weekly sync' });
      await waitFor(() => expect(commands.processingStatus).toHaveBeenCalled());
      expect(screen.queryByRole('progressbar')).not.toBeInTheDocument();
    });
  });
});

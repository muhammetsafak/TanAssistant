import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '../../i18n';
import { commands } from '../../ipc/bindings';
import { fail, ok, renderWithProviders } from '../../test-utils';
import { newVersion, settings, upToDate } from './fixtures';
import { UpdateSettings } from './UpdateSettings';

vi.mock('../../ipc/bindings', async () => {
  const { stubEvents } = await import('../../test-events');
  return {
    commands: {
      appVersion: vi.fn(),
      getSettings: vi.fn(),
      updateSettings: vi.fn(),
      checkForUpdate: vi.fn(),
      downloadUpdate: vi.fn(),
      installUpdateAndRestart: vi.fn(),
      openReleasePage: vi.fn(),
      recordingStatus: vi.fn(),
    },
    events: stubEvents(),
  };
});

describe('UpdateSettings', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
    vi.resetAllMocks();
    vi.mocked(commands.appVersion).mockResolvedValue('1.2.3');
    vi.mocked(commands.getSettings).mockResolvedValue(ok(settings));
    vi.mocked(commands.updateSettings).mockImplementation((patch) =>
      Promise.resolve(ok({ ...settings, autoCheckUpdates: patch.autoCheckUpdates ?? true })),
    );
    vi.mocked(commands.recordingStatus).mockResolvedValue(ok({ state: 'idle', active: null }));
  });

  it('shows the running version and the automatic-check switch', async () => {
    renderWithProviders(<UpdateSettings />);

    expect(await screen.findByText('Mevcut sürüm: 1.2.3')).toBeInTheDocument();
    expect(
      await screen.findByRole('checkbox', { name: 'Güncellemeleri otomatik denetle' }),
    ).toBeChecked();
  });

  it('saves the automatic-check switch', async () => {
    renderWithProviders(<UpdateSettings />);

    await userEvent.click(
      await screen.findByRole('checkbox', { name: 'Güncellemeleri otomatik denetle' }),
    );

    await waitFor(() =>
      expect(commands.updateSettings).toHaveBeenCalledWith(
        expect.objectContaining({ autoCheckUpdates: false }),
      ),
    );
  });

  it('says the app is up to date after a manual check', async () => {
    vi.mocked(commands.checkForUpdate).mockResolvedValue(ok(upToDate));
    renderWithProviders(<UpdateSettings />);

    await userEvent.click(await screen.findByRole('button', { name: 'Güncellemeleri denetle' }));

    expect(await screen.findByText('Tan güncel; sürüm 1.2.3 en yeni sürüm.')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'İndir ve kur' })).not.toBeInTheDocument();
  });

  it('offers the download when a newer version exists', async () => {
    vi.mocked(commands.checkForUpdate).mockResolvedValue(ok(newVersion));
    renderWithProviders(<UpdateSettings />);

    await userEvent.click(await screen.findByRole('button', { name: 'Güncellemeleri denetle' }));

    expect(await screen.findByText('Yeni sürüm 1.4.0 hazır.')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'İndir ve kur' })).toBeEnabled();
  });

  it('shows a toast and an error line when the manual check fails', async () => {
    vi.mocked(commands.checkForUpdate).mockResolvedValue(fail('update_failed') as never);
    renderWithProviders(<UpdateSettings />);

    await userEvent.click(await screen.findByRole('button', { name: 'Güncellemeleri denetle' }));

    expect(await screen.findByRole('alert')).toHaveTextContent('Güncelleme denetlenemedi');
    // The card keeps the failure visible after the toast is gone.
    expect(screen.getAllByText(/Güncelleme denetlenemedi/)).toHaveLength(2);
  });

  it('disables the check button while a check runs', async () => {
    let finish: (value: ReturnType<typeof ok>) => void = () => undefined;
    vi.mocked(commands.checkForUpdate).mockReturnValue(
      new Promise((resolve) => {
        finish = resolve as typeof finish;
      }),
    );
    renderWithProviders(<UpdateSettings />);

    await userEvent.click(await screen.findByRole('button', { name: 'Güncellemeleri denetle' }));

    expect(await screen.findByRole('button', { name: 'Denetleniyor…' })).toBeDisabled();
    finish(ok(upToDate));
    expect(await screen.findByRole('button', { name: 'Güncellemeleri denetle' })).toBeEnabled();
  });
});

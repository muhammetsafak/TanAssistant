import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '../i18n';
import { commands } from '../ipc/bindings';
import { ok, renderWithProviders } from '../test-utils';
import { Sidebar } from './Sidebar';
import { TopBar } from './TopBar';

vi.mock('../ipc/bindings', async () => {
  const { stubEvents } = await import('../test-events');
  return {
    commands: {
      appVersion: vi.fn(),
      getSettings: vi.fn(),
      updateSettings: vi.fn(),
      recordingStatus: vi.fn(),
      getStorageUsage: vi.fn(),
    },
    events: stubEvents(),
  };
});

function settings(uiLanguage: 'tr' | 'en') {
  return {
    uiLanguage,
    defaultNoteLanguage: 'tr',
    defaultSpeechLanguage: 'auto',
    frameIntervalMs: 3000,
    frameChangeThreshold: 8,
    autoProcessAfterRecording: true,
    audioRetentionDays: null,
    consentNoticeAcknowledged: true,
    autoCheckUpdates: true,
  };
}

describe('language switching', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
    vi.mocked(commands.appVersion).mockResolvedValue('0.1.0');
    vi.mocked(commands.recordingStatus).mockResolvedValue(ok({ state: 'idle', active: null }));
    vi.mocked(commands.getSettings).mockResolvedValue(ok(settings('tr')));
    vi.mocked(commands.updateSettings).mockResolvedValue(ok(settings('en')));
    vi.mocked(commands.getStorageUsage).mockResolvedValue(
      ok({ totalBytes: 2_576_980_378, audioBytes: 1, frameBytes: 1, modelBytes: 1 }),
    );
  });

  it('changes the labels and persists ui_language', async () => {
    const user = userEvent.setup();
    renderWithProviders(
      <>
        <Sidebar />
        <TopBar />
      </>,
    );
    expect(screen.getByRole('link', { name: 'Kütüphane' })).toBeInTheDocument();

    await user.selectOptions(screen.getByLabelText('Dil'), 'en');

    expect(await screen.findByRole('link', { name: 'Library' })).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'Settings' })).toBeInTheDocument();
    expect(screen.queryByRole('link', { name: 'Kütüphane' })).not.toBeInTheDocument();
    expect(screen.getByLabelText('Language')).toHaveValue('en');
    await waitFor(() => expect(commands.updateSettings).toHaveBeenCalledWith({ uiLanguage: 'en' }));
    expect(document.documentElement.lang).toBe('en');
  });

  it('reverts the language and reports the error when persisting fails', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.updateSettings).mockResolvedValue({
      status: 'error',
      error: { code: 'internal', message: 'disk full' },
    });
    renderWithProviders(<TopBar />);
    await vi.waitFor(() => expect(commands.getSettings).toHaveBeenCalled());

    await user.selectOptions(screen.getByLabelText('Dil'), 'en');

    expect(await screen.findByRole('alert')).toHaveTextContent('Beklenmeyen bir hata oluştu.');
    await waitFor(() => expect(screen.getByLabelText('Dil')).toHaveValue('tr'));
  });

  it('marks the current page, shows the Tan mark and the disk usage card', async () => {
    renderWithProviders(<Sidebar />, '/recordings');

    const current = await screen.findByRole('link', { name: 'Kütüphane' });
    expect(current).toHaveAttribute('aria-current', 'page');
    expect(screen.getByRole('link', { name: 'Ayarlar' })).not.toHaveAttribute('aria-current');
    // The logo is decorative: the word "Tan" next to it names the brand.
    expect(document.querySelector('.sidebar__logo')).toHaveAttribute('alt', '');
    expect(await screen.findByTestId('sidebar-disk')).toHaveTextContent('Disk kullanımı');
    expect(screen.getByTestId('sidebar-disk')).toHaveTextContent('2,4 GB');
  });

  it('leaves the disk card out while the size is unknown', async () => {
    vi.mocked(commands.getStorageUsage).mockResolvedValue(
      ok({ totalBytes: null, audioBytes: null, frameBytes: null, modelBytes: null }),
    );
    renderWithProviders(<Sidebar />);

    await screen.findByRole('link', { name: 'Kütüphane' });
    await vi.waitFor(() => expect(commands.getStorageUsage).toHaveBeenCalled());
    expect(screen.queryByTestId('sidebar-disk')).not.toBeInTheDocument();
  });

  it('reads the disk size again when the recording state changes', async () => {
    vi.mocked(commands.recordingStatus).mockResolvedValue(
      ok({
        state: 'recording',
        active: {
          sessionId: 's1',
          title: 'T',
          startedAt: 1,
          elapsedMs: 1000,
          microphoneLevel: null,
          systemLevel: null,
          frameCount: 0,
        },
      }),
    );
    const { queryClient } = renderWithProviders(<Sidebar />);
    await screen.findByTestId('sidebar-disk');
    expect(commands.getStorageUsage).toHaveBeenCalledTimes(1);

    // The recording is stopped: the status flips to idle and the size is stale.
    vi.mocked(commands.recordingStatus).mockResolvedValue(ok({ state: 'idle', active: null }));
    await queryClient.invalidateQueries({ queryKey: ['recording', 'status'] });

    await waitFor(() => expect(commands.getStorageUsage).toHaveBeenCalledTimes(2));
  });
});

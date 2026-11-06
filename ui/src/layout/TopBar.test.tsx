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
  };
}

describe('language switching', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
    vi.mocked(commands.appVersion).mockResolvedValue('0.1.0');
    vi.mocked(commands.recordingStatus).mockResolvedValue(ok({ state: 'idle', active: null }));
    vi.mocked(commands.getSettings).mockResolvedValue(ok(settings('tr')));
    vi.mocked(commands.updateSettings).mockResolvedValue(ok(settings('en')));
  });

  it('changes the labels and persists ui_language', async () => {
    const user = userEvent.setup();
    renderWithProviders(
      <>
        <Sidebar />
        <TopBar />
      </>,
    );
    expect(screen.getByRole('link', { name: 'Kayıtlar' })).toBeInTheDocument();

    await user.selectOptions(screen.getByLabelText('Dil'), 'en');

    expect(await screen.findByRole('link', { name: 'Recordings' })).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'Settings' })).toBeInTheDocument();
    expect(screen.queryByRole('link', { name: 'Kayıtlar' })).not.toBeInTheDocument();
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
});

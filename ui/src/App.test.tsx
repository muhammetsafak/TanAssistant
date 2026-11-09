import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import App from './App';
import i18n from './i18n';
import { commands } from './ipc/bindings';
import { ok } from './test-utils';

vi.mock('./ipc/bindings', async () => {
  const { stubEvents } = await import('./test-events');
  return {
    commands: {
      appVersion: vi.fn(),
      getSettings: vi.fn(),
      updateSettings: vi.fn(),
      listVariables: vi.fn(),
      secretStoreStatus: vi.fn(),
      listSecrets: vi.fn(),
      recordingStatus: vi.fn(),
      recordingCapabilities: vi.fn(),
      listAudioDevices: vi.fn(),
      listDisplays: vi.fn(),
      listSessions: vi.fn(),
      getStorageUsage: vi.fn(),
    },
    events: stubEvents(),
  };
});

const settings = {
  uiLanguage: 'tr' as const,
  defaultNoteLanguage: 'tr',
  defaultSpeechLanguage: 'auto',
  frameIntervalMs: 3000,
  frameChangeThreshold: 8,
  autoProcessAfterRecording: true,
  audioRetentionDays: null,
  consentNoticeAcknowledged: true,
  autoCheckUpdates: true,
};

describe('App shell', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
    window.location.hash = '#/';
    vi.mocked(commands.appVersion).mockResolvedValue('1.2.3');
    vi.mocked(commands.recordingStatus).mockResolvedValue(ok({ state: 'idle', active: null }));
    vi.mocked(commands.recordingCapabilities).mockResolvedValue(
      ok({ screenCaptureAvailable: true, screenUnavailableReason: null }),
    );
    vi.mocked(commands.listAudioDevices).mockResolvedValue(ok([]));
    vi.mocked(commands.listDisplays).mockResolvedValue(ok([]));
    vi.mocked(commands.listSessions).mockResolvedValue(
      ok({ items: [], total: 0, offset: 0, limit: 20 }),
    );
    vi.mocked(commands.getStorageUsage).mockResolvedValue(
      ok({ totalBytes: 1_073_741_824, audioBytes: 1, frameBytes: 1, modelBytes: 1 }),
    );
    vi.mocked(commands.getSettings).mockResolvedValue(ok(settings));
    vi.mocked(commands.listVariables).mockResolvedValue(ok([]));
    vi.mocked(commands.listSecrets).mockResolvedValue(ok([]));
    vi.mocked(commands.secretStoreStatus).mockResolvedValue({
      backend: 'keyring',
      available: true,
      locked: false,
      masterPasswordSet: false,
    });
  });

  it('shows the navigation and the version from the app_version command', async () => {
    render(<App />);

    const nav = await screen.findByRole('navigation', { name: 'Ana gezinme' });
    expect(nav).toHaveTextContent('Kütüphane');
    expect(nav).toHaveTextContent('AI Sağlayıcıları');
    expect(nav).toHaveTextContent('Credentials');
    expect(nav).toHaveTextContent('Ayarlar');
    expect(await screen.findByText('Sürüm 1.2.3')).toBeInTheDocument();
    expect(await screen.findByTestId('sidebar-disk')).toHaveTextContent('1 GB');
  });

  it('navigates to the credentials page from the sidebar', async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.click(await screen.findByRole('link', { name: 'Credentials' }));

    expect(
      await screen.findByRole('heading', { level: 1, name: 'Credentials' }),
    ).toBeInTheDocument();
    expect(await screen.findByRole('heading', { name: "Variable'lar" })).toBeInTheDocument();
  });
});

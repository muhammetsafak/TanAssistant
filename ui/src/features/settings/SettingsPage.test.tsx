import { act, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '../../i18n';
import { commands } from '../../ipc/bindings';
import SettingsPage from '../../pages/SettingsPage';
import { emitEvent } from '../../test-events';
import { fail, ok, renderWithProviders } from '../../test-utils';

vi.mock('../../ipc/bindings', async () => {
  const { stubEvents } = await import('../../test-events');
  return {
    commands: {
      appVersion: vi.fn(),
      checkForUpdate: vi.fn(),
      recordingStatus: vi.fn(),
      getSettings: vi.fn(),
      updateSettings: vi.fn(),
      listWhisperModels: vi.fn(),
      downloadWhisperModel: vi.fn(),
      selectWhisperModel: vi.fn(),
      deleteWhisperModel: vi.fn(),
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

const MB = 1024 * 1024;

const models = [
  {
    id: 'small-q5',
    displayName: 'Whisper small',
    sizeBytes: 190 * MB,
    quantization: 'q5_1',
    recommended: false,
    installed: true,
    selected: true,
    partialBytes: 0,
  },
  {
    id: 'large-v3-turbo',
    displayName: 'Whisper large-v3-turbo',
    sizeBytes: 574 * MB,
    quantization: 'q5_0',
    recommended: true,
    installed: false,
    selected: false,
    partialBytes: 0,
  },
];

describe('SettingsPage', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
    vi.mocked(commands.appVersion).mockResolvedValue('1.2.3');
    vi.mocked(commands.recordingStatus).mockResolvedValue(ok({ state: 'idle', active: null }));
    vi.mocked(commands.checkForUpdate).mockResolvedValue(
      ok({
        available: false,
        currentVersion: '1.2.3',
        version: null,
        notes: null,
        pubDate: null,
        canSelfUpdate: true,
        releaseUrl: 'https://example.test/releases',
      }),
    );
    vi.mocked(commands.getSettings).mockResolvedValue(ok(settings));
    vi.mocked(commands.updateSettings).mockImplementation((patch) =>
      Promise.resolve(
        ok({
          ...settings,
          ...(patch.audioRetention
            ? { audioRetentionDays: patch.audioRetention.days ?? null }
            : {}),
          ...(patch.frameIntervalMs ? { frameIntervalMs: patch.frameIntervalMs } : {}),
        }),
      ),
    );
    vi.mocked(commands.listWhisperModels).mockResolvedValue(ok(models));
    vi.mocked(commands.getStorageUsage).mockResolvedValue(
      ok({
        totalBytes: 3 * 1024 * MB,
        audioBytes: 512 * MB,
        frameBytes: null,
        modelBytes: 190 * MB,
      }),
    );
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it('saves the frame interval in milliseconds and rejects out-of-range values', async () => {
    const user = userEvent.setup();
    renderWithProviders(<SettingsPage />);

    const interval = await screen.findByLabelText('Kare aralığı (saniye)');
    expect(interval).toHaveValue(3);
    const form = interval.closest('form')!;
    await user.clear(interval);
    await user.type(interval, '90');
    await user.click(within(form).getByRole('button', { name: 'Kaydet' }));
    expect(await screen.findByText(/0.5 ile 60 arasında/)).toBeInTheDocument();
    expect(commands.updateSettings).not.toHaveBeenCalled();

    await user.clear(interval);
    await user.type(interval, '2.5');
    await user.click(within(form).getByRole('button', { name: 'Kaydet' }));

    await waitFor(() =>
      expect(commands.updateSettings).toHaveBeenCalledWith({
        frameIntervalMs: 2500,
        frameChangeThreshold: 8,
      }),
    );
  });

  it('switches the audio retention from forever to a number of days', async () => {
    const user = userEvent.setup();
    renderWithProviders(<SettingsPage />);

    const keepForever = await screen.findByRole('radio', { name: 'Süresiz sakla' });
    expect(keepForever).toBeChecked();
    await user.click(screen.getByRole('radio', { name: /ses dosyalarını sil/ }));
    const days = screen.getByLabelText('Gün sayısı');
    await user.clear(days);
    await user.type(days, '14');
    const form = days.closest('form')!;
    await user.click(within(form).getByRole('button', { name: 'Kaydet' }));

    await waitFor(() =>
      expect(commands.updateSettings).toHaveBeenCalledWith({ audioRetention: { days: 14 } }),
    );
  });

  it('shows the disk usage, with an em dash where the size is unknown', async () => {
    renderWithProviders(<SettingsPage />);

    const usage = (await screen.findByText('Toplam')).closest('dl')!;
    expect(usage).toHaveTextContent('3 GB');
    expect(usage).toHaveTextContent('512 MB');
    expect(within(usage).getByText('Kareler').nextElementSibling).toHaveTextContent('—');
  });

  it('downloads a model with progress and chooses an installed one', async () => {
    const user = userEvent.setup();
    let finish: (value: unknown) => void = () => undefined;
    vi.mocked(commands.downloadWhisperModel).mockReturnValue(
      new Promise((resolve) => {
        finish = resolve;
      }) as never,
    );
    renderWithProviders(<SettingsPage />);

    expect(await screen.findByText('Kullanımda')).toBeInTheDocument();
    await user.click(await screen.findByRole('button', { name: 'İndir: Whisper large-v3-turbo' }));
    expect(commands.downloadWhisperModel).toHaveBeenCalledWith('large-v3-turbo');

    act(() => {
      emitEvent('whisperModelDownloadEvent', {
        modelId: 'large-v3-turbo',
        downloaded: 287 * MB,
        total: 574 * MB,
      });
    });
    expect(
      screen.getByRole('progressbar', { name: 'Whisper large-v3-turbo indirme ilerlemesi' }),
    ).toHaveValue(50);
    expect(screen.getByText(/%50/)).toBeInTheDocument();

    vi.mocked(commands.listWhisperModels).mockResolvedValue(
      ok([models[0]!, { ...models[1]!, installed: true }]),
    );
    vi.mocked(commands.selectWhisperModel).mockResolvedValue(ok(null));
    finish(ok({ ...models[1], installed: true }));
    await user.click(await screen.findByRole('button', { name: 'Kullan: Whisper large-v3-turbo' }));
    expect(commands.selectWhisperModel).toHaveBeenCalledWith('large-v3-turbo');
  });

  it('reports a failed download by its code', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.downloadWhisperModel).mockResolvedValue(fail('download_failed') as never);
    renderWithProviders(<SettingsPage />);

    await user.click(await screen.findByRole('button', { name: 'İndir: Whisper large-v3-turbo' }));

    expect(await screen.findByRole('alert')).toHaveTextContent('İndirme başarısız oldu');
  });
});

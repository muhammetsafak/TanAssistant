import { act, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { Route, Routes } from 'react-router';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '../../i18n';
import { commands } from '../../ipc/bindings';
import { emitEvent } from '../../test-events';
import { fail, ok, renderWithProviders } from '../../test-utils';
import { RecordingControl } from './RecordingControl';

vi.mock('../../ipc/bindings', async () => {
  const { stubEvents } = await import('../../test-events');
  return {
    commands: {
      getSettings: vi.fn(),
      recordingStatus: vi.fn(),
      recordingCapabilities: vi.fn(),
      listAudioDevices: vi.fn(),
      listDisplays: vi.fn(),
      startRecording: vi.fn(),
      stopRecording: vi.fn(),
      pauseRecording: vi.fn(),
      resumeRecording: vi.fn(),
      displayThumbnail: vi.fn(),
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
};

const active = {
  sessionId: 's9',
  title: 'Design review',
  startedAt: 1,
  elapsedMs: 5000,
  microphoneLevel: 0.04,
  systemLevel: null,
  frameCount: 2,
};

function renderControl() {
  return renderWithProviders(
    <Routes>
      <Route path="/" element={<RecordingControl />} />
      <Route path="recordings/:sessionId" element={<p>detail page</p>} />
    </Routes>,
  );
}

describe('RecordingControl', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
    vi.mocked(commands.getSettings).mockResolvedValue(ok(settings));
    vi.mocked(commands.recordingStatus).mockResolvedValue(ok({ state: 'idle', active: null }));
    vi.mocked(commands.recordingCapabilities).mockResolvedValue(
      ok({ screenCaptureAvailable: true, screenUnavailableReason: null }),
    );
    vi.mocked(commands.listAudioDevices).mockResolvedValue(
      ok([
        { id: 'mic1', name: 'Built-in mic', isDefault: true },
        { id: 'mic2', name: 'USB headset', isDefault: false },
      ]),
    );
    vi.mocked(commands.listDisplays).mockResolvedValue(
      ok([{ id: 'd1', name: 'Display 1', width: 1920, height: 1080, isPrimary: true }]),
    );
    vi.mocked(commands.displayThumbnail).mockResolvedValue(
      ok({
        available: false,
        dataUrl: null,
        width: null,
        height: null,
        unavailableReason: 'unsupported',
      }),
    );
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it('starts a recording with the chosen title, languages and devices', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.startRecording).mockResolvedValue(
      ok({ sessionId: 's9', title: 'Design review', startedAt: 1 }),
    );
    renderControl();

    await user.type(await screen.findByLabelText('Başlık'), '  Design review ');
    await user.selectOptions(screen.getByLabelText('Konuşma dili'), 'en');
    expect(screen.getByLabelText('Not dili')).toHaveDisplayValue('Türkçe');
    await user.selectOptions(await screen.findByLabelText('Mikrofon'), 'mic2');
    await user.click(await screen.findByRole('button', { name: /Ekran/ }));
    await user.click(await screen.findByRole('radio', { name: 'Display 1' }));
    await user.click(screen.getByRole('button', { name: 'Kaydı başlat' }));

    expect(commands.startRecording).toHaveBeenCalledWith({
      title: 'Design review',
      speechLanguage: 'en',
      noteLanguage: 'tr',
      microphoneDeviceId: 'mic2',
      displayId: 'd1',
    });
  });

  it('explains why there is no screen capture on Wayland and records audio only', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.recordingCapabilities).mockResolvedValue(
      ok({ screenCaptureAvailable: false, screenUnavailableReason: 'wayland_unsupported' }),
    );
    vi.mocked(commands.startRecording).mockResolvedValue(
      ok({ sessionId: 's9', title: 'x', startedAt: 1 }),
    );
    renderControl();

    expect(
      await screen.findByText(/Wayland oturumunda ekran görüntüsü yakalanamıyor/),
    ).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Ekran/ })).not.toBeInTheDocument();
    expect(commands.listDisplays).not.toHaveBeenCalled();

    await user.click(screen.getByRole('button', { name: 'Kaydı başlat' }));
    expect(commands.startRecording).toHaveBeenCalledWith(
      expect.objectContaining({ displayId: null }),
    );
  });

  it('reports a refused start by its code', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.startRecording).mockResolvedValue(
      fail('permission_denied', 'Allow the microphone in Settings') as never,
    );
    renderControl();

    await user.click(await screen.findByRole('button', { name: 'Kaydı başlat' }));

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'İşletim sistemi mikrofon ya da ekran erişimini reddetti',
    );
  });

  it('shows the live numbers of a running recording and follows the progress events', async () => {
    vi.mocked(commands.recordingStatus).mockResolvedValue(ok({ state: 'recording', active }));
    renderControl();

    expect(await screen.findByText('Kayıt sürüyor')).toBeInTheDocument();
    expect(screen.getByText('Design review')).toBeInTheDocument();
    expect(screen.getByRole('timer')).toHaveTextContent('00:05');
    expect(screen.getByRole('meter', { name: 'Mikrofon seviyesi' })).toBeInTheDocument();
    expect(screen.getByText('sinyal yok')).toBeInTheDocument();

    act(() => {
      emitEvent('recordingProgressEvent', {
        sessionId: 's9',
        elapsedMs: 65_000,
        microphoneLevel: 0.2,
        systemLevel: 0.1,
        frameCount: 7,
      });
    });

    expect(screen.getByRole('timer')).toHaveTextContent('01:05');
    expect(screen.getByText('7')).toBeInTheDocument();
    expect(screen.queryByText('sinyal yok')).not.toBeInTheDocument();

    act(() => {
      emitEvent('recordingWarningEvent', { sessionId: 's9', message: 'Loopback device changed' });
    });
    expect(screen.getByText('Loopback device changed')).toBeInTheDocument();
  });

  it('stops the recording and opens its session', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.recordingStatus).mockResolvedValue(ok({ state: 'recording', active }));
    vi.mocked(commands.stopRecording).mockResolvedValue(
      ok({
        sessionId: 's9',
        status: 'recorded',
        durationMs: 60_000,
        tracks: [],
        frameCount: 3,
        droppedFrames: 0,
        warnings: [],
      }),
    );
    renderControl();

    await user.click(await screen.findByRole('button', { name: 'Kaydı durdur' }));

    expect(commands.stopRecording).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(screen.getByText('detail page')).toBeInTheDocument());
  });

  describe('pausing', () => {
    const pausedStatus = {
      state: 'paused',
      active: { ...active, elapsedMs: 42_000, microphoneLevel: 0.3, systemLevel: 0.2 },
    };

    it('pauses a running recording and shows the paused state', async () => {
      const user = userEvent.setup();
      vi.mocked(commands.recordingStatus).mockResolvedValue(ok({ state: 'recording', active }));
      vi.mocked(commands.pauseRecording).mockImplementation(() => {
        vi.mocked(commands.recordingStatus).mockResolvedValue(ok(pausedStatus));
        return Promise.resolve(ok(pausedStatus));
      });
      renderControl();

      await user.click(await screen.findByRole('button', { name: 'Kaydı duraklat' }));

      expect(commands.pauseRecording).toHaveBeenCalledTimes(1);
      expect(await screen.findByText('Kayıt duraklatıldı')).toBeInTheDocument();
      expect(screen.getByText('Duraklatıldı')).toBeInTheDocument();
      expect(screen.getByRole('timer')).toHaveTextContent('00:42');
      expect(screen.getByRole('button', { name: 'Kaydı devam ettir' })).toBeInTheDocument();
      expect(screen.getByRole('button', { name: 'Kaydı durdur' })).toBeEnabled();
      expect(screen.getAllByText('duraklatıldı')).toHaveLength(2);
      for (const meter of screen.getAllByRole('meter')) {
        expect(meter).toHaveAttribute('value', '0');
      }
    });

    it('resumes a paused recording', async () => {
      const user = userEvent.setup();
      vi.mocked(commands.recordingStatus).mockResolvedValue(ok(pausedStatus));
      const running = ok({ state: 'recording', active: pausedStatus.active });
      vi.mocked(commands.resumeRecording).mockImplementation(() => {
        vi.mocked(commands.recordingStatus).mockResolvedValue(running);
        return Promise.resolve(running);
      });
      renderControl();

      await user.click(await screen.findByRole('button', { name: 'Kaydı devam ettir' }));

      expect(commands.resumeRecording).toHaveBeenCalledTimes(1);
      expect(await screen.findByText('Kayıt sürüyor')).toBeInTheDocument();
      expect(screen.getByRole('button', { name: 'Kaydı duraklat' })).toBeInTheDocument();
    });

    it('keeps the clock still when a late progress tick arrives while paused', async () => {
      vi.mocked(commands.recordingStatus).mockResolvedValue(ok(pausedStatus));
      renderControl();
      expect(await screen.findByRole('timer')).toHaveTextContent('00:42');

      act(() => {
        emitEvent('recordingProgressEvent', {
          sessionId: 's9',
          elapsedMs: 90_000,
          microphoneLevel: 0.5,
          systemLevel: 0.5,
          frameCount: 9,
        });
      });

      expect(screen.getByRole('timer')).toHaveTextContent('00:42');
    });

    it('follows a pause made from the tray', async () => {
      vi.mocked(commands.recordingStatus).mockResolvedValue(ok({ state: 'recording', active }));
      renderControl();
      expect(await screen.findByText('Kayıt sürüyor')).toBeInTheDocument();

      vi.mocked(commands.recordingStatus).mockResolvedValue(ok(pausedStatus));
      act(() => {
        emitEvent('recordingStateEvent', { state: 'paused', sessionId: 's9' });
      });

      expect(await screen.findByText('Kayıt duraklatıldı')).toBeInTheDocument();
      expect(screen.getByRole('timer')).toHaveTextContent('00:42');
    });

    it('keeps the recording warnings across a pause and resume', async () => {
      vi.mocked(commands.recordingStatus).mockResolvedValue(ok({ state: 'recording', active }));
      renderControl();
      await screen.findByText('Kayıt sürüyor');
      act(() => {
        emitEvent('recordingWarningEvent', { sessionId: 's9', message: 'Loopback device changed' });
      });
      act(() => {
        emitEvent('recordingStateEvent', { state: 'recording', sessionId: 's9' });
      });
      expect(screen.getByText('Loopback device changed')).toBeInTheDocument();
    });

    it('stops a paused recording', async () => {
      const user = userEvent.setup();
      vi.mocked(commands.recordingStatus).mockResolvedValue(ok(pausedStatus));
      vi.mocked(commands.stopRecording).mockResolvedValue(
        ok({
          sessionId: 's9',
          status: 'recorded',
          durationMs: 42_000,
          tracks: [],
          frameCount: 3,
          droppedFrames: 0,
          warnings: [],
        }),
      );
      renderControl();

      await user.click(await screen.findByRole('button', { name: 'Kaydı durdur' }));

      await waitFor(() => expect(screen.getByText('detail page')).toBeInTheDocument());
    });

    it('re-reads the status and explains when the recording was not in the expected state', async () => {
      const user = userEvent.setup();
      vi.mocked(commands.recordingStatus).mockResolvedValue(ok({ state: 'recording', active }));
      vi.mocked(commands.pauseRecording).mockResolvedValue(
        fail('already_paused', 'already paused') as never,
      );
      renderControl();

      vi.mocked(commands.recordingStatus).mockResolvedValue(ok(pausedStatus));
      await user.click(await screen.findByRole('button', { name: 'Kaydı duraklat' }));

      expect(await screen.findByRole('alert')).toHaveTextContent('Kayıt zaten duraklatılmış.');
      expect(await screen.findByText('Kayıt duraklatıldı')).toBeInTheDocument();
    });

    it('does not let the recording be stopped while a pause is being applied', async () => {
      const user = userEvent.setup();
      vi.mocked(commands.recordingStatus).mockResolvedValue(ok({ state: 'recording', active }));
      let finish: () => void = () => undefined;
      vi.mocked(commands.pauseRecording).mockImplementation(
        () =>
          new Promise((resolve) => {
            finish = () => {
              vi.mocked(commands.recordingStatus).mockResolvedValue(ok(pausedStatus));
              resolve(ok(pausedStatus));
            };
          }),
      );
      renderControl();

      await user.click(await screen.findByRole('button', { name: 'Kaydı duraklat' }));

      expect(screen.getByRole('button', { name: 'Kaydı durdur' })).toBeDisabled();
      act(() => finish());
      await screen.findByText('Kayıt duraklatıldı');
      expect(screen.getByRole('button', { name: 'Kaydı durdur' })).toBeEnabled();
    });
  });
});

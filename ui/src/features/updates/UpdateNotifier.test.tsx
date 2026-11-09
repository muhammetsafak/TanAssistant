import { act, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '../../i18n';
import { commands } from '../../ipc/bindings';
import { emitEvent } from '../../test-events';
import { fail, ok, renderWithProviders } from '../../test-utils';
import { newVersion, settings, upToDate } from './fixtures';
import { useRecordingEvents } from '../recording/useRecordingEvents';
import { useProcessingRun } from '../sessions/api';
import { UpdateNotifier } from './UpdateNotifier';

vi.mock('../../ipc/bindings', async () => {
  const { stubEvents } = await import('../../test-events');
  return {
    commands: {
      getSettings: vi.fn(),
      updateSettings: vi.fn(),
      checkForUpdate: vi.fn(),
      downloadUpdate: vi.fn(),
      installUpdateAndRestart: vi.fn(),
      openReleasePage: vi.fn(),
      recordingStatus: vi.fn(),
      processSession: vi.fn(),
    },
    events: stubEvents(),
  };
});

/** The app keeps the recording status fresh from the top bar; this stands in for it. */
function WithRecordingEvents() {
  useRecordingEvents();
  return <UpdateNotifier />;
}

/** Starts a processing run, like the library does. */
function StartRun() {
  const run = useProcessingRun();
  return (
    <button type="button" onClick={() => run.mutate({ sessionId: 's1', mode: 'process' })}>
      start run
    </button>
  );
}

/** A call that stays pending until the test settles it. */
function deferred<T>() {
  let resolve: (value: T) => void = () => undefined;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

async function downloadAndWait() {
  vi.mocked(commands.downloadUpdate).mockResolvedValue(ok(null));
  await userEvent.click(await screen.findByRole('button', { name: 'İndir ve kur' }));
  return screen.findByRole('button', { name: 'Yeniden başlat' });
}

describe('UpdateNotifier', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
    vi.resetAllMocks();
    vi.mocked(commands.getSettings).mockResolvedValue(ok(settings));
    vi.mocked(commands.checkForUpdate).mockResolvedValue(ok(newVersion));
    vi.mocked(commands.recordingStatus).mockResolvedValue(ok({ state: 'idle', active: null }));
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('shows nothing when the app is up to date', async () => {
    vi.mocked(commands.checkForUpdate).mockResolvedValue(ok(upToDate));
    renderWithProviders(<UpdateNotifier />);

    await waitFor(() => expect(commands.checkForUpdate).toHaveBeenCalledTimes(1));
    expect(screen.queryByRole('region', { name: 'Güncelleme bildirimi' })).not.toBeInTheDocument();
  });

  it('announces a new version on start', async () => {
    renderWithProviders(<UpdateNotifier />);

    const banner = await screen.findByRole('region', { name: 'Güncelleme bildirimi' });
    expect(banner).toHaveTextContent('Yeni sürüm 1.4.0');
    expect(screen.getByRole('button', { name: 'İndir ve kur' })).toBeEnabled();
  });

  it('does not check on start when the automatic check is off', async () => {
    vi.mocked(commands.getSettings).mockResolvedValue(ok({ ...settings, autoCheckUpdates: false }));
    renderWithProviders(<UpdateNotifier />);

    await waitFor(() => expect(commands.getSettings).toHaveBeenCalled());
    await act(() => Promise.resolve());
    expect(commands.checkForUpdate).not.toHaveBeenCalled();
    expect(screen.queryByRole('region', { name: 'Güncelleme bildirimi' })).not.toBeInTheDocument();
  });

  it('checks again every 24 hours while it stays open', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(commands.checkForUpdate).mockResolvedValue(ok(upToDate));
    renderWithProviders(<UpdateNotifier />);

    await waitFor(() => expect(commands.checkForUpdate).toHaveBeenCalledTimes(1));
    await act(() => vi.advanceTimersByTimeAsync(24 * 60 * 60 * 1000));

    await waitFor(() => expect(commands.checkForUpdate).toHaveBeenCalledTimes(2));
  });

  it('fails silently when the automatic check fails', async () => {
    vi.mocked(commands.checkForUpdate).mockResolvedValue(fail('update_failed') as never);
    renderWithProviders(<UpdateNotifier />);

    await waitFor(() => expect(commands.checkForUpdate).toHaveBeenCalledTimes(1));
    await act(() => Promise.resolve());
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    expect(screen.queryByRole('region', { name: 'Güncelleme bildirimi' })).not.toBeInTheDocument();
  });

  it('can be dismissed, and the same version does not come back this session', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    renderWithProviders(<UpdateNotifier />);
    await userEvent.click(await screen.findByRole('button', { name: 'Kapat' }));
    expect(screen.queryByRole('region', { name: 'Güncelleme bildirimi' })).not.toBeInTheDocument();

    await act(() => vi.advanceTimersByTimeAsync(24 * 60 * 60 * 1000));
    await waitFor(() => expect(commands.checkForUpdate).toHaveBeenCalledTimes(2));

    expect(screen.queryByRole('region', { name: 'Güncelleme bildirimi' })).not.toBeInTheDocument();
  });

  it('shows the download progress', async () => {
    const download = deferred<ReturnType<typeof ok>>();
    vi.mocked(commands.downloadUpdate).mockReturnValue(download.promise as never);
    renderWithProviders(<UpdateNotifier />);

    await userEvent.click(await screen.findByRole('button', { name: 'İndir ve kur' }));
    expect(await screen.findByText('Güncelleme indiriliyor…')).toBeInTheDocument();

    act(() => emitEvent('updateDownloadProgress', { downloaded: 40, total: 100 }));

    const bar = await screen.findByRole('progressbar', { name: 'Güncelleme indirme durumu' });
    expect(bar).toHaveAttribute('value', '40');
    expect(screen.getByText('%40')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'İndir ve kur' })).not.toBeInTheDocument();

    download.resolve(ok(null));
    expect(await screen.findByRole('button', { name: 'Yeniden başlat' })).toBeEnabled();
    expect(screen.getByText(/kuruluma hazır/)).toBeInTheDocument();
  });

  it('restarts into the update when idle', async () => {
    vi.mocked(commands.installUpdateAndRestart).mockResolvedValue(ok(null));
    renderWithProviders(<UpdateNotifier />);

    await userEvent.click(await downloadAndWait());

    expect(commands.installUpdateAndRestart).toHaveBeenCalledTimes(1);
  });

  it('keeps restart off while a recording runs, and says why', async () => {
    vi.mocked(commands.recordingStatus).mockResolvedValue(
      ok({
        state: 'recording',
        active: {
          sessionId: 's1',
          startedAt: 0,
          elapsedMs: 5000,
        } as never,
      }),
    );
    renderWithProviders(<UpdateNotifier />);

    const restart = await downloadAndWait();

    await waitFor(() => expect(restart).toBeDisabled());
    expect(restart).toHaveAccessibleDescription(/kayıt ya da işleme sürerken/i);
  });

  it('turns restart on again when the recording ends', async () => {
    vi.mocked(commands.recordingStatus).mockResolvedValue(
      ok({ state: 'recording', active: { sessionId: 's1' } as never }),
    );
    renderWithProviders(<WithRecordingEvents />);
    const restart = await downloadAndWait();
    await waitFor(() => expect(restart).toBeDisabled());

    vi.mocked(commands.recordingStatus).mockResolvedValue(ok({ state: 'idle', active: null }));
    act(() => emitEvent('recordingStateEvent', { state: 'idle', sessionId: 's1' }));

    await waitFor(() => expect(restart).toBeEnabled());
  });

  it('keeps restart off while a processing run is going on, and on again after it', async () => {
    const run = deferred<ReturnType<typeof ok>>();
    vi.mocked(commands.processSession).mockReturnValue(run.promise as never);
    renderWithProviders(
      <>
        <StartRun />
        <UpdateNotifier />
      </>,
    );
    const restart = await downloadAndWait();
    expect(restart).toBeEnabled();

    await userEvent.click(screen.getByRole('button', { name: 'start run' }));

    await waitFor(() => expect(restart).toBeDisabled());
    expect(restart).toHaveAccessibleDescription(/kayıt ya da işleme sürerken/i);

    run.resolve(ok({}));
    await waitFor(() => expect(restart).toBeEnabled());
  });

  it('goes back to the download when the backend lost the package', async () => {
    vi.mocked(commands.installUpdateAndRestart).mockResolvedValueOnce(
      fail('update_not_found') as never,
    );
    renderWithProviders(<UpdateNotifier />);

    await userEvent.click(await downloadAndWait());

    expect(await screen.findByRole('alert')).toHaveTextContent('Önce güncellemeleri denetle');
    expect(await screen.findByRole('button', { name: 'İndir ve kur' })).toBeEnabled();
    expect(screen.queryByRole('button', { name: 'Yeniden başlat' })).not.toBeInTheDocument();
  });

  it('explains a refusal of the install and lets it be tried again', async () => {
    vi.mocked(commands.installUpdateAndRestart).mockResolvedValueOnce(
      fail('update_blocked') as never,
    );
    renderWithProviders(<UpdateNotifier />);

    await userEvent.click(await downloadAndWait());

    const toast = await screen.findByRole('alert');
    expect(toast).toHaveTextContent('Kayıt ya da işleme sürerken güncelleme kurulamaz');
    expect(await screen.findByRole('button', { name: 'Yeniden başlat' })).toBeEnabled();
  });

  it('waits for the restart when the backend holds it back', async () => {
    vi.mocked(commands.installUpdateAndRestart).mockResolvedValueOnce(
      fail('update_restart_pending') as never,
    );
    renderWithProviders(<UpdateNotifier />);

    await userEvent.click(await downloadAndWait());

    expect(await screen.findByText(/Güncelleme kuruldu/)).toBeInTheDocument();
    const restart = screen.getByRole('button', { name: 'Yeniden başlat' });
    expect(restart).toBeEnabled();

    vi.mocked(commands.installUpdateAndRestart).mockResolvedValue(ok(null));
    await userEvent.click(restart);
    expect(commands.installUpdateAndRestart).toHaveBeenCalledTimes(2);
    expect(commands.downloadUpdate).toHaveBeenCalledTimes(1);
  });

  it('shows a toast and offers the download again when it fails', async () => {
    vi.mocked(commands.downloadUpdate).mockResolvedValue(fail('update_failed') as never);
    renderWithProviders(<UpdateNotifier />);

    await userEvent.click(await screen.findByRole('button', { name: 'İndir ve kur' }));

    expect(await screen.findByRole('alert')).toHaveTextContent('Güncelleme denetlenemedi');
    expect(await screen.findByRole('button', { name: 'İndir ve kur' })).toBeEnabled();
  });

  it('offers only the release page when the install cannot update itself', async () => {
    vi.mocked(commands.checkForUpdate).mockResolvedValue(
      ok({ ...newVersion, canSelfUpdate: false }),
    );
    vi.mocked(commands.openReleasePage).mockResolvedValue(ok(null));
    renderWithProviders(<UpdateNotifier />);

    const open = await screen.findByRole('button', { name: 'Release sayfasını aç' });
    expect(screen.queryByRole('button', { name: 'İndir ve kur' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Yeniden başlat' })).not.toBeInTheDocument();

    await userEvent.click(open);
    expect(commands.openReleasePage).toHaveBeenCalledTimes(1);
  });
});

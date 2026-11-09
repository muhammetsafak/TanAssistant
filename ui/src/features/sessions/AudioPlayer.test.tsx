import { act, fireEvent, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { createRef } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '../../i18n';
import { renderWithProviders } from '../../test-utils';
import type { AudioTrack } from './api';
import { AudioPlayer, type AudioPlayerHandle } from './AudioPlayer';
import { createPlayhead } from './playhead';

vi.mock('@tauri-apps/api/core', () => ({
  convertFileSrc: (path: string) => `asset://localhost/${encodeURIComponent(path)}`,
}));

const microphone: AudioTrack = {
  kind: 'microphone',
  path: 'audio/microphone.wav',
  sampleRate: 16000,
  durationMs: 60_000,
  deletedAt: null,
};
const system: AudioTrack = { ...microphone, kind: 'system', path: 'audio/system.wav' };

/** jsdom keeps no media clock, so the elements get a plain, settable `currentTime`. */
function installFakeClock(elements: HTMLAudioElement[]) {
  for (const element of elements) {
    let time = 0;
    Object.defineProperty(element, 'currentTime', {
      configurable: true,
      get: () => time,
      set: (value: number) => {
        time = value;
      },
    });
  }
}

function renderPlayer(tracks: AudioTrack[], playhead = createPlayhead()) {
  const ref = createRef<AudioPlayerHandle>();
  const view = renderWithProviders(
    <AudioPlayer ref={ref} sessionDir="/data/sessions/s1" tracks={tracks} playhead={playhead} />,
  );
  const elements = Array.from(view.container.querySelectorAll('audio'));
  installFakeClock(elements);
  return { ...view, ref, playhead, elements };
}

describe('AudioPlayer', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
    vi.spyOn(HTMLMediaElement.prototype, 'play').mockResolvedValue(undefined);
    vi.spyOn(HTMLMediaElement.prototype, 'pause').mockImplementation(() => undefined);
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('follows the microphone: it reports the position and puts a drifted track back in step', () => {
    const { elements, playhead } = renderPlayer([system, microphone]);
    const [follower, main] = elements;
    if (!follower || !main) throw new Error('both tracks should be loaded');

    main.currentTime = 10;
    follower.currentTime = 10.1;
    fireEvent.timeUpdate(main);
    // Within 0.15 s: left alone.
    expect(follower.currentTime).toBe(10.1);
    expect(playhead.get()).toBe(10_000);

    follower.currentTime = 10.5;
    fireEvent.timeUpdate(main);
    expect(follower.currentTime).toBe(10);
  });

  it('seeks every track and clamps to the length of the recording', () => {
    const { elements, ref } = renderPlayer([microphone, system]);

    act(() => ref.current?.seek(30_000));
    for (const element of elements) expect(element.currentTime).toBe(30);

    act(() => ref.current?.seek(999_000));
    for (const element of elements) expect(element.currentTime).toBe(60);

    act(() => ref.current?.seek(-5));
    for (const element of elements) expect(element.currentTime).toBe(0);
  });

  it('seeks from the slider, which the browser also moves with the arrow keys', () => {
    const { elements } = renderPlayer([microphone]);
    const slider = screen.getByRole('slider', { name: 'Oynatma konumu' });
    // One arrow key press is one `step` (1 s); the browser reports it as a change.
    expect(slider).toHaveAttribute('step', '1000');

    fireEvent.change(slider, { target: { value: '42000' } });

    expect(elements[0]?.currentTime).toBe(42);
    expect(slider).toHaveAttribute('aria-valuetext', '00:42 / 01:00');
  });

  it('stops every track when the main one ends, and starts over from the end', async () => {
    const user = userEvent.setup();
    const { elements, ref } = renderPlayer([microphone, system]);

    await user.click(screen.getByRole('button', { name: 'Oynat' }));
    expect(screen.getByRole('button', { name: 'Duraklat' })).toBeInTheDocument();

    act(() => ref.current?.seek(60_000));
    fireEvent.ended(elements[0] as HTMLAudioElement);
    expect(screen.getByRole('button', { name: 'Oynat' })).toBeInTheDocument();
    expect(HTMLMediaElement.prototype.pause).toHaveBeenCalledTimes(2);

    await user.click(screen.getByRole('button', { name: 'Oynat' }));
    for (const element of elements) expect(element.currentTime).toBe(0);
  });

  it('reports a refused play() and stays paused', async () => {
    const user = userEvent.setup();
    vi.spyOn(HTMLMediaElement.prototype, 'play').mockRejectedValue(new Error('NotAllowedError'));
    renderPlayer([microphone]);

    await user.click(screen.getByRole('button', { name: 'Oynat' }));

    expect(await screen.findByRole('alert')).toHaveTextContent('Ses başlatılamadı');
    expect(screen.getByRole('button', { name: 'Oynat' })).toBeInTheDocument();
  });

  it('refuses a track outside the audio folder', () => {
    renderPlayer([{ ...microphone, path: '../secrets/other.wav' }]);

    expect(screen.getByRole('alert')).toHaveTextContent('oynatılamıyor');
    expect(screen.queryByRole('button', { name: 'Oynat' })).not.toBeInTheDocument();
  });
});

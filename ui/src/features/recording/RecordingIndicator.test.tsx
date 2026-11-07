import { act, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '../../i18n';
import { commands } from '../../ipc/bindings';
import { emitEvent } from '../../test-events';
import { ok, renderWithProviders } from '../../test-utils';
import { RecordingIndicator } from './RecordingIndicator';

vi.mock('../../ipc/bindings', async () => {
  const { stubEvents } = await import('../../test-events');
  return { commands: { recordingStatus: vi.fn() }, events: stubEvents() };
});

const active = {
  sessionId: 's1',
  title: 'Standup',
  startedAt: 1,
  elapsedMs: 61_000,
  microphoneLevel: null,
  systemLevel: null,
  frameCount: 0,
};

describe('RecordingIndicator', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it('shows nothing while idle', async () => {
    vi.mocked(commands.recordingStatus).mockResolvedValue(ok({ state: 'idle', active: null }));
    renderWithProviders(<RecordingIndicator />);
    await vi.waitFor(() => expect(commands.recordingStatus).toHaveBeenCalled());
    expect(screen.queryByRole('link')).not.toBeInTheDocument();
  });

  it('says the recording is paused and ignores progress ticks while it is', async () => {
    vi.mocked(commands.recordingStatus).mockResolvedValue(ok({ state: 'paused', active }));
    renderWithProviders(<RecordingIndicator />);

    const link = await screen.findByRole('link', { name: /Kayıt duraklatıldı/ });
    expect(link).toHaveTextContent('01:01');

    act(() => {
      emitEvent('recordingProgressEvent', {
        sessionId: 's1',
        elapsedMs: 120_000,
        microphoneLevel: null,
        systemLevel: null,
        frameCount: 0,
      });
    });
    expect(link).toHaveTextContent('01:01');
  });

  it('shows a running recording', async () => {
    vi.mocked(commands.recordingStatus).mockResolvedValue(ok({ state: 'recording', active }));
    renderWithProviders(<RecordingIndicator />);
    expect(await screen.findByRole('link', { name: /Kayıt sürüyor/ })).toBeInTheDocument();
  });
});

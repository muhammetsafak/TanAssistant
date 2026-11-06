import { waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { commands, events } from '../../ipc/bindings';
import { emitEvent } from '../../test-events';
import { fail, ok, renderWithProviders } from '../../test-utils';
import { sessionDetail } from './fixtures';
import { useAutoProcess } from './useAutoProcess';

vi.mock('../../ipc/bindings', async () => {
  const { stubEvents } = await import('../../test-events');
  return {
    commands: {
      getSettings: vi.fn(),
      getSessionDetail: vi.fn(),
      estimateTranscription: vi.fn(),
      listProviderProfiles: vi.fn(),
      processSession: vi.fn(),
    },
    events: stubEvents(),
  };
});

function Host() {
  useAutoProcess();
  return null;
}

function stubSettings(autoProcessAfterRecording: boolean) {
  vi.mocked(commands.getSettings).mockResolvedValue(ok({ autoProcessAfterRecording }) as never);
}

function stubEstimate(overrides: Record<string, unknown>) {
  vi.mocked(commands.estimateTranscription).mockResolvedValue(
    ok({
      engine: 'local_whisper',
      providerKind: null,
      estimatedSeconds: 120,
      backend: 'cpu',
      audioSeconds: 600,
      speechSeconds: 300,
      ...overrides,
    }) as never,
  );
}

/** A recording ended: the backend announces `idle` with the id of the session it left behind. */
async function recordingEnded() {
  await waitFor(() => expect(events.recordingStateEvent.listen).toHaveBeenCalled());
  emitEvent('recordingStateEvent', { state: 'idle', sessionId: 's1' });
}

describe('useAutoProcess', () => {
  beforeEach(() => {
    stubSettings(true);
    vi.mocked(commands.getSessionDetail).mockResolvedValue(
      ok(sessionDetail({ session: { ...sessionDetail().session, status: 'recorded' } })) as never,
    );
    stubEstimate({});
    vi.mocked(commands.listProviderProfiles).mockResolvedValue(ok([]));
    vi.mocked(commands.processSession).mockResolvedValue(ok({}) as never);
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it('starts processing a recorded session when the setting is on', async () => {
    renderWithProviders(<Host />);
    await recordingEnded();

    await waitFor(() => expect(commands.processSession).toHaveBeenCalledWith('s1', null));
  });

  it('does nothing when the setting is off', async () => {
    stubSettings(false);
    renderWithProviders(<Host />);
    await recordingEnded();

    await waitFor(() => expect(commands.getSettings).toHaveBeenCalled());
    expect(commands.getSessionDetail).not.toHaveBeenCalled();
    expect(commands.processSession).not.toHaveBeenCalled();
  });

  it('ignores a state change that is not the end of a recording', async () => {
    renderWithProviders(<Host />);
    await waitFor(() => expect(events.recordingStateEvent.listen).toHaveBeenCalled());
    emitEvent('recordingStateEvent', { state: 'recording', sessionId: 's1' });

    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(commands.getSettings).not.toHaveBeenCalled();
    expect(commands.processSession).not.toHaveBeenCalled();
  });

  it('skips a session that is not "recorded" any more', async () => {
    vi.mocked(commands.getSessionDetail).mockResolvedValue(ok(sessionDetail()) as never);
    renderWithProviders(<Host />);
    await recordingEnded();

    await waitFor(() => expect(commands.getSessionDetail).toHaveBeenCalled());
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(commands.processSession).not.toHaveBeenCalled();
  });

  it('leaves a long CPU forecast to the user when a cloud transcription profile exists', async () => {
    stubEstimate({ estimatedSeconds: 3_600 });
    vi.mocked(commands.listProviderProfiles).mockResolvedValue(
      ok([{ id: 'p1', capabilities: { transcription: true } }]) as never,
    );
    renderWithProviders(<Host />);
    await recordingEnded();

    await waitFor(() => expect(commands.listProviderProfiles).toHaveBeenCalled());
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(commands.processSession).not.toHaveBeenCalled();
  });

  it('still starts a long CPU forecast when no cloud transcription profile exists', async () => {
    stubEstimate({ estimatedSeconds: 3_600 });
    vi.mocked(commands.listProviderProfiles).mockResolvedValue(
      ok([{ id: 'p1', capabilities: { transcription: false } }]) as never,
    );
    renderWithProviders(<Host />);
    await recordingEnded();

    await waitFor(() => expect(commands.processSession).toHaveBeenCalledWith('s1', null));
  });

  it('starts anyway when there is no forecast, so the run can report the reason', async () => {
    vi.mocked(commands.estimateTranscription).mockResolvedValue(fail('model_missing') as never);
    renderWithProviders(<Host />);
    await recordingEnded();

    await waitFor(() => expect(commands.processSession).toHaveBeenCalledWith('s1', null));
  });
});

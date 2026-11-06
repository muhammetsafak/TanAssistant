import { useQueryClient } from '@tanstack/react-query';

import { events } from '../../ipc/bindings';
import { useTauriEvent } from '../../ipc/useTauriEvent';
import { settingsQueryKey } from '../settings/useSettings';
import { ipc, type AppSettings } from '../../ipc/client';
import { estimateQueryOptions, sessionDetailQueryOptions, useProcessingRun } from './api';
import { LONG_LOCAL_TRANSCRIPTION_SECONDS } from './longTranscription';
import { profilesQueryOptions } from '../providers/api';

/**
 * Honours "process automatically when a recording ends" for every way a recording can end (the
 * window or the tray). A long local forecast on a CPU-only machine with a cloud profile at hand is
 * the exception: the session is left for the user, whose detail screen offers the choice (ADR-011).
 */
export function useAutoProcess(): void {
  const queryClient = useQueryClient();
  const run = useProcessingRun();

  useTauriEvent(events.recordingStateEvent, ({ state, sessionId }) => {
    if (state !== 'idle') return;
    void (async () => {
      try {
        const settings = await queryClient.fetchQuery<AppSettings>({
          queryKey: settingsQueryKey,
          queryFn: ipc.getSettings,
          staleTime: 30_000,
        });
        if (!settings.autoProcessAfterRecording) return;

        // Always fresh: a cached copy may predate the recording's end.
        const detail = await queryClient.fetchQuery({
          ...sessionDetailQueryOptions(sessionId),
          staleTime: 0,
        });
        if (detail.session.status !== 'recorded') return;

        if (await shouldLeaveToUser(queryClient, sessionId)) return;
        run.mutate({ sessionId, mode: 'process' });
      } catch {
        // Nothing to report here: the session stays "recorded" and its screen offers "Process".
      }
    })();
  });
}

async function shouldLeaveToUser(
  queryClient: ReturnType<typeof useQueryClient>,
  sessionId: string,
): Promise<boolean> {
  try {
    const forecast = await queryClient.fetchQuery(estimateQueryOptions(sessionId));
    if (
      forecast.engine !== 'local_whisper' ||
      forecast.backend !== 'cpu' ||
      forecast.estimatedSeconds === null ||
      forecast.estimatedSeconds <= LONG_LOCAL_TRANSCRIPTION_SECONDS
    ) {
      return false;
    }
    const profiles = await queryClient.fetchQuery({
      ...profilesQueryOptions(),
      staleTime: 30_000,
    });
    return profiles.some((profile) => profile.capabilities.transcription);
  } catch {
    // No forecast (no model yet, no audio): start anyway and let the run report the reason.
    return false;
  }
}

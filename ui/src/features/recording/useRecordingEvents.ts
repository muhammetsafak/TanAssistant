import { useState } from 'react';
import { useQueryClient } from '@tanstack/react-query';

import { events, type RecordingProgressEvent, type RecordingStateEvent } from '../../ipc/bindings';
import { useTauriEvent } from '../../ipc/useTauriEvent';
import { sessionKeys } from '../sessions/keys';
import { recordingKeys } from './api';

export interface RecordingWarnings {
  sessionId: string;
  messages: string[];
}

export interface RecordingEvents {
  /** The latest progress tick of the running recording; `null` before the first one. */
  live: RecordingProgressEvent | null;
  /** Non-fatal problems reported by the capture sources of the running recording. */
  warnings: string[];
}

/**
 * Live numbers and warnings of the running recording (Phase 08 events). A start or stop, from the
 * window or from the tray, refreshes the status and the session list.
 */
export function useRecordingEvents(): RecordingEvents {
  const queryClient = useQueryClient();
  const [live, setLive] = useState<RecordingProgressEvent | null>(null);
  const [warnings, setWarnings] = useState<RecordingWarnings | null>(null);

  useTauriEvent(events.recordingProgressEvent, (progress) => setLive(progress));

  useTauriEvent(events.recordingStateEvent, (event: RecordingStateEvent) => {
    if (event.state !== 'recording') {
      setLive(null);
    } else {
      setWarnings(null);
    }
    void queryClient.invalidateQueries({ queryKey: recordingKeys.status });
    void queryClient.invalidateQueries({ queryKey: sessionKeys.all });
  });

  useTauriEvent(events.recordingWarningEvent, ({ sessionId, message }) =>
    setWarnings((current) => {
      if (current?.sessionId !== sessionId) return { sessionId, messages: [message] };
      if (current.messages.includes(message)) return current;
      return { sessionId, messages: [...current.messages, message] };
    }),
  );

  return { live, warnings: warnings?.messages ?? [] };
}

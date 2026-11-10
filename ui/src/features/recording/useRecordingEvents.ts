import { useState } from 'react';
import { useQueryClient } from '@tanstack/react-query';

import {
  events,
  type RecordingProgressEvent,
  type RecordingStateEvent,
  type RecordingWarningCodeDto,
} from '../../ipc/bindings';
import { useTauriEvent } from '../../ipc/useTauriEvent';
import { sessionKeys } from '../sessions/keys';
import { recordingKeys } from './api';

/** One non-fatal problem; `code` tells the UI which wording to use, `message` is the fallback. */
export interface RecordingWarning {
  code: RecordingWarningCodeDto;
  /** `microphone` or `system` when the warning concerns one audio track. */
  track: string | null;
  message: string;
}

interface SessionWarnings {
  sessionId: string;
  items: RecordingWarning[];
}

/** Two warnings with the same code, track and message are the same problem. */
export function warningKey(warning: RecordingWarning): string {
  return `${warning.code}|${warning.track ?? ''}|${warning.message}`;
}

export interface RecordingEvents {
  /** The latest progress tick of the running recording; `null` before the first one. */
  live: RecordingProgressEvent | null;
  /** Non-fatal problems reported by the capture sources of the running recording. */
  warnings: RecordingWarning[];
}

/**
 * Live numbers and warnings of the running recording (Phase 08 events). A start or stop, from the
 * window or from the tray, refreshes the status and the session list.
 */
export function useRecordingEvents(): RecordingEvents {
  const queryClient = useQueryClient();
  const [live, setLive] = useState<RecordingProgressEvent | null>(null);
  const [warnings, setWarnings] = useState<SessionWarnings | null>(null);

  useTauriEvent(events.recordingProgressEvent, (progress) => setLive(progress));

  useTauriEvent(events.recordingStateEvent, (event: RecordingStateEvent) => {
    // Pausing, ending: the next numbers come from the status read, not from an old tick.
    setLive(null);
    // A resume announces `recording` too; only a different session drops the old warnings.
    if (event.state === 'recording') {
      setWarnings((current) => (current?.sessionId === event.sessionId ? current : null));
    }
    void queryClient.invalidateQueries({ queryKey: recordingKeys.status });
    void queryClient.invalidateQueries({ queryKey: sessionKeys.all });
  });

  useTauriEvent(events.recordingWarningEvent, ({ sessionId, code, track, message }) => {
    const warning: RecordingWarning = { code, track: track ?? null, message };
    setWarnings((current) => {
      if (current?.sessionId !== sessionId) return { sessionId, items: [warning] };
      const key = warningKey(warning);
      if (current.items.some((item) => warningKey(item) === key)) return current;
      return { sessionId, items: [...current.items, warning] };
    });
  });

  return { live, warnings: warnings?.items ?? [] };
}

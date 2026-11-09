import {
  keepPreviousData,
  queryOptions,
  useMutation,
  useMutationState,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query';
import { convertFileSrc } from '@tauri-apps/api/core';

import {
  commands,
  type ActionItemDto_Serialize,
  type AudioTrackDto_Serialize,
  type FrameDto_Serialize,
  type NoteDto_Serialize,
  type ProcessingJobDto_Serialize,
  type ProcessingStatusDto_Serialize,
  type SearchHitDto_Serialize,
  type SessionDetailDto_Serialize,
  type SessionDto_Serialize,
  type SessionListItemDto_Serialize,
  type SessionSearchResultDto_Serialize,
  type SessionStatusDto,
  type TranscriptionEstimateDto_Serialize,
  type TranscriptLineDto_Serialize,
} from '../../ipc/bindings';
import {
  ProcessingCommandError,
  unwrapProcessing,
  unwrapSessions,
  unwrapTranscription,
} from '../../ipc/errors';
import { sessionKeys, type SessionListQuery } from './keys';

export type Session = SessionDto_Serialize;
export type SessionListItem = SessionListItemDto_Serialize;
export type SessionDetail = SessionDetailDto_Serialize;
export type SessionSearchResult = SessionSearchResultDto_Serialize;
export type SearchHit = SearchHitDto_Serialize;
export type SessionStatus = SessionStatusDto;
export type Frame = FrameDto_Serialize;
export type AudioTrack = AudioTrackDto_Serialize;
export type Note = NoteDto_Serialize;
export type ActionItem = ActionItemDto_Serialize;
export type TranscriptLine = TranscriptLineDto_Serialize;
export type ProcessingJob = ProcessingJobDto_Serialize;
export type ProcessingStatus = ProcessingStatusDto_Serialize;
export type TranscriptionEstimate = TranscriptionEstimateDto_Serialize;

export const SESSION_STATUSES: readonly SessionStatus[] = [
  'recording',
  'recorded',
  'processing',
  'processed',
  'failed',
];

export const PAGE_SIZE = 20;

export function useSessionList(query: SessionListQuery) {
  return useQuery({
    queryKey: sessionKeys.list(query),
    queryFn: () =>
      unwrapSessions(
        commands.listSessions(
          { status: query.status },
          { offset: query.offset, limit: query.limit },
        ),
      ),
    placeholderData: keepPreviousData,
  });
}

/** Full-text search; `text` is already trimmed and debounced by the caller. */
export function useSessionSearch(text: string) {
  return useQuery({
    queryKey: sessionKeys.search(text),
    queryFn: () => unwrapSessions(commands.searchSessions(text)),
    enabled: text !== '',
    placeholderData: keepPreviousData,
  });
}

export function sessionDetailQueryOptions(sessionId: string) {
  return queryOptions({
    queryKey: sessionKeys.detail(sessionId),
    queryFn: () => unwrapSessions(commands.getSessionDetail(sessionId)),
  });
}

export function useSessionDetail(sessionId: string) {
  return useQuery(sessionDetailQueryOptions(sessionId));
}

/**
 * Marks an action item done or not done. The checkbox flips at once; a refused call puts it back.
 */
export function useToggleActionItem(sessionId: string) {
  const queryClient = useQueryClient();
  const detailKey = sessionKeys.detail(sessionId);
  return useMutation({
    mutationFn: ({ actionItemId, done }: { actionItemId: string; done: boolean }) =>
      unwrapSessions(commands.toggleActionItem(actionItemId, done)),
    onMutate: async ({ actionItemId, done }) => {
      await queryClient.cancelQueries({ queryKey: detailKey });
      const previous = queryClient.getQueryData<SessionDetail>(detailKey);
      if (previous?.note) {
        queryClient.setQueryData<SessionDetail>(detailKey, {
          ...previous,
          note: {
            ...previous.note,
            actionItems: previous.note.actionItems.map((item) =>
              item.id === actionItemId ? { ...item, done } : item,
            ),
          },
        });
      }
      return { previous };
    },
    onError: (_error, _variables, context) => {
      if (context?.previous) queryClient.setQueryData(detailKey, context.previous);
    },
    onSettled: async () => {
      // The list shows "done / total" per session.
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: detailKey }),
        queryClient.invalidateQueries({ queryKey: [...sessionKeys.all, 'list'] }),
      ]);
    },
  });
}

export function useExportSession() {
  return useMutation({
    mutationFn: ({ sessionId, destination }: { sessionId: string; destination: string | null }) =>
      unwrapSessions(commands.exportSessionMarkdown(sessionId, destination)),
  });
}

/** Mixes the session's audio tracks into one WAV file at `destination` (from the save dialog). */
export function useExportSessionAudio() {
  return useMutation({
    mutationFn: ({ sessionId, destination }: { sessionId: string; destination: string }) =>
      unwrapSessions(commands.exportSessionAudio(sessionId, destination)),
  });
}

export function useDeleteSession() {
  const queryClient = useQueryClient();
  return useMutation({
    // The command itself refuses a deletion that was not confirmed; the caller asks first.
    mutationFn: (sessionId: string) => unwrapSessions(commands.deleteSession(sessionId, true)),
    onSuccess: async (_data, sessionId) => {
      queryClient.removeQueries({ queryKey: sessionKeys.detail(sessionId) });
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: sessionKeys.all }),
        queryClient.invalidateQueries({ queryKey: sessionKeys.storage }),
      ]);
    },
  });
}

// ---------------------------------------------------------------------------------------
// Processing
// ---------------------------------------------------------------------------------------

/** While a run is in progress the job is re-read this often (events only carry percentages). */
const RUNNING_POLL_MS = 2000;

export function useProcessingStatus(sessionId: string) {
  return useQuery({
    queryKey: sessionKeys.processing(sessionId),
    queryFn: () => unwrapProcessing(commands.processingStatus(sessionId)),
    refetchInterval: (query) => (query.state.data?.running ? RUNNING_POLL_MS : false),
  });
}

export type ProcessingMode = 'process' | 'retry';

const RUN_KEY = ['processing-run'] as const;

/**
 * Runs the pipeline for a session: `process` from scratch (replaces an earlier note), `retry`
 * continues a failed or cancelled run. The call returns when the run ends, so its mutation
 * outlives the screen that started it; everything it changes is refreshed from here.
 */
export function useProcessingRun() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationKey: RUN_KEY,
    mutationFn: ({
      sessionId,
      mode,
      transcriptionProfileId = null,
    }: {
      sessionId: string;
      mode: ProcessingMode;
      /** Transcribe with this cloud profile in this run only; the general assignment is untouched. */
      transcriptionProfileId?: string | null;
    }) =>
      unwrapProcessing(
        mode === 'retry'
          ? commands.retryProcessing(sessionId, transcriptionProfileId)
          : commands.processSession(sessionId, transcriptionProfileId),
      ),
    onMutate: ({ sessionId }) => {
      void queryClient.invalidateQueries({ queryKey: sessionKeys.processing(sessionId) });
    },
    onSettled: async (_data, _error, { sessionId }) => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: sessionKeys.processing(sessionId) }),
        queryClient.invalidateQueries({ queryKey: sessionKeys.detail(sessionId) }),
        queryClient.invalidateQueries({ queryKey: [...sessionKeys.all, 'list'] }),
        queryClient.invalidateQueries({ queryKey: [...sessionKeys.all, 'search'] }),
      ]);
    },
  });
}

/** Whether a run of `sessionId` started from anywhere in the app (even the auto-start) is going on. */
export function useIsProcessingRunPending(sessionId: string): boolean {
  const pending = useMutationState({
    filters: { mutationKey: RUN_KEY, status: 'pending' },
    select: (mutation) =>
      (mutation.state.variables as { sessionId: string } | undefined)?.sessionId,
  });
  return pending.includes(sessionId);
}

/**
 * The error of the latest run of `sessionId` still in the cache, when that run failed (it survives
 * a remount). A later run that succeeded or is still going on clears it.
 */
export function useLastProcessingError(sessionId: string): ProcessingCommandError | null {
  const runs = useMutationState({
    filters: { mutationKey: RUN_KEY },
    select: (mutation) => ({
      sessionId: (mutation.state.variables as { sessionId: string } | undefined)?.sessionId,
      status: mutation.state.status,
      error: mutation.state.error,
      at: mutation.state.submittedAt,
    }),
  });
  // A run refused because another one is going on says nothing about how that one ended.
  const mine = runs
    .filter(
      (run) =>
        run.sessionId === sessionId &&
        !(run.error instanceof ProcessingCommandError && run.error.code === 'already_running'),
    )
    .sort((a, b) => a.at - b.at);
  const latest = mine[mine.length - 1];
  return latest?.status === 'error' && latest.error instanceof ProcessingCommandError
    ? latest.error
    : null;
}

export function useCancelProcessing() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (sessionId: string) => unwrapProcessing(commands.cancelProcessing(sessionId)),
    onSettled: (_data, _error, sessionId) =>
      queryClient.invalidateQueries({ queryKey: sessionKeys.processing(sessionId) }),
  });
}

/**
 * The forecast of the local transcription time (ADR-011). The first call runs a short
 * calibration on the selected model, so it can take a few seconds; the answer is kept for a while.
 */
export function estimateQueryOptions(sessionId: string) {
  return queryOptions({
    queryKey: sessionKeys.estimate(sessionId),
    queryFn: () => unwrapTranscription(commands.estimateTranscription(sessionId)),
    staleTime: 60_000,
  });
}

export function useTranscriptionEstimate(sessionId: string, enabled: boolean) {
  return useQuery({ ...estimateQueryOptions(sessionId), enabled });
}

// ---------------------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------------------

/**
 * The webview URL of a frame image through Tauri's asset protocol. Only a path relative to the
 * session folder, without parent-directory steps, is accepted, so a frame can never point outside
 * the application data directory. `null` when the URL cannot be built (no Tauri runtime).
 */
export function frameImageUrl(sessionDir: string, relativePath: string): string | null {
  const segments = relativePath.split(/[\\/]/);
  const unsafe =
    relativePath === '' ||
    /^([a-zA-Z]:|[\\/])/.test(relativePath) ||
    segments.some((segment) => segment === '..');
  if (unsafe) return null;
  const separator = sessionDir.includes('\\') && !sessionDir.includes('/') ? '\\' : '/';
  const base = sessionDir.replace(/[\\/]+$/, '');
  try {
    return convertFileSrc(`${base}${separator}${segments.join(separator)}`);
  } catch {
    return null;
  }
}

// ---------------------------------------------------------------------------------------
// Audio
// ---------------------------------------------------------------------------------------

/**
 * The webview URL of a session's audio track through Tauri's asset protocol. Like `frameImageUrl`
 * it takes a path relative to the session folder, and it only accepts the `audio/` folder, so a
 * track can never point at anything else. `null` when the URL cannot be built (no Tauri runtime).
 */
export function audioUrl(sessionDir: string, relativePath: string): string | null {
  if (!/^audio[\\/]/.test(relativePath)) return null;
  return frameImageUrl(sessionDir, relativePath);
}

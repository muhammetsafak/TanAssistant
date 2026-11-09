import { useCallback } from 'react';
import { useQuery, useQueryClient, type QueryClient } from '@tanstack/react-query';

import { useToast } from '../../components/ui/toast-context';
import { events } from '../../ipc/bindings';
import { ipc, IpcCommandError, type UpdateInfo } from '../../ipc/client';
import { useTauriEvent } from '../../ipc/useTauriEvent';
import { useRecordingStatus } from '../recording/api';
import { useIsAnyProcessingRunPending } from '../sessions/api';

export type { UpdateInfo };

/** The auto-check repeats this often while the application stays open. */
export const UPDATE_CHECK_INTERVAL_MS = 24 * 60 * 60 * 1000;

/**
 * Where the update flow stands. `ready` is a downloaded, signature-checked update waiting for a
 * restart; `restartPending` is one the backend installed but could not restart for yet.
 */
export type UpdatePhase =
  'idle' | 'checking' | 'downloading' | 'ready' | 'installing' | 'restartPending';

export interface UpdateProgress {
  downloaded: number;
  total: number | null;
}

/** A failure the user started (manual check, download, install); shown by its error code. */
export interface UpdateFailure {
  kind: 'check' | 'download' | 'install';
  error: unknown;
}

export interface UpdateState {
  /** The latest check result; `null` until one succeeded. */
  info: UpdateInfo | null;
  phase: UpdatePhase;
  progress: UpdateProgress | null;
  failure: UpdateFailure | null;
  /** Versions whose notification the user closed; they do not come back in this session. */
  dismissedVersions: string[];
}

const INITIAL_STATE: UpdateState = {
  info: null,
  phase: 'idle',
  progress: null,
  failure: null,
  dismissedVersions: [],
};

const stateKey = ['updates', 'state'] as const;

function readState(queryClient: QueryClient): UpdateState {
  return queryClient.getQueryData<UpdateState>(stateKey) ?? INITIAL_STATE;
}

function patchState(queryClient: QueryClient, patch: Partial<UpdateState>): void {
  queryClient.setQueryData<UpdateState>(stateKey, { ...readState(queryClient), ...patch });
}

/**
 * The update flow's state. It lives in the query cache, so the notification (always mounted) and
 * the settings card (open on one page) see the same download, whichever started it.
 */
export function useUpdateState(): UpdateState {
  const { data } = useQuery({
    queryKey: stateKey,
    queryFn: () => INITIAL_STATE,
    initialData: INITIAL_STATE,
    staleTime: Infinity,
    gcTime: Infinity,
  });
  return data;
}

/**
 * Feeds the download progress events into the flow's state. Mount it once, where it is always
 * present (the notifier), not in every component that reads the state.
 */
export function useUpdateProgressEvents(): void {
  const queryClient = useQueryClient();
  useTauriEvent(events.updateDownloadProgress, ({ downloaded, total }) => {
    if (readState(queryClient).phase !== 'downloading') return;
    patchState(queryClient, { progress: { downloaded, total } });
  });
}

export interface UpdateActions {
  /** `manual` shows a failure as a toast; the automatic check fails silently. */
  check: (manual: boolean) => Promise<void>;
  download: () => Promise<void>;
  install: () => Promise<void>;
  openReleasePage: () => Promise<void>;
  /** Hides the notification of `version` for the rest of this session. */
  dismiss: (version: string) => void;
}

/** The commands of the update flow. Every one is a no-op when the flow is not in a fitting phase. */
export function useUpdateActions(): UpdateActions {
  const queryClient = useQueryClient();
  const toast = useToast();

  const check = useCallback(
    async (manual: boolean) => {
      const before = readState(queryClient);
      if (before.phase !== 'idle') return;
      patchState(queryClient, { phase: 'checking', failure: manual ? null : before.failure });
      try {
        const info = await ipc.checkForUpdate();
        patchState(queryClient, { info, phase: 'idle', failure: null });
      } catch (error) {
        patchState(queryClient, {
          phase: 'idle',
          failure: manual ? { kind: 'check', error } : before.failure,
        });
        if (manual) toast.error(error);
      }
    },
    [queryClient, toast],
  );

  const download = useCallback(async () => {
    const { info, phase } = readState(queryClient);
    if (phase !== 'idle' || !info?.available || !info.canSelfUpdate) return;
    patchState(queryClient, { phase: 'downloading', progress: null, failure: null });
    try {
      await ipc.downloadUpdate();
      patchState(queryClient, { phase: 'ready', progress: null });
    } catch (error) {
      patchState(queryClient, {
        phase: 'idle',
        progress: null,
        failure: { kind: 'download', error },
      });
      toast.error(error);
    }
  }, [queryClient, toast]);

  const install = useCallback(async () => {
    const { phase } = readState(queryClient);
    if (phase !== 'ready' && phase !== 'restartPending') return;
    patchState(queryClient, { phase: 'installing', failure: null });
    try {
      // On success the application restarts, so there is nothing to do afterwards.
      await ipc.installUpdateAndRestart();
    } catch (error) {
      if (error instanceof IpcCommandError && error.code === 'update_restart_pending') {
        patchState(queryClient, { phase: 'restartPending' });
        return;
      }
      // The backend no longer holds the package: go back to where the download can be started
      // again instead of leaving a restart button that can only fail.
      const gone = error instanceof IpcCommandError && error.code === 'update_not_found';
      patchState(queryClient, {
        phase: gone ? 'idle' : phase,
        failure: { kind: 'install', error },
      });
      toast.error(error);
    }
  }, [queryClient, toast]);

  const openReleasePage = useCallback(async () => {
    try {
      await ipc.openReleasePage();
    } catch (error) {
      toast.error(error);
    }
  }, [toast]);

  const dismiss = useCallback(
    (version: string) => {
      const { dismissedVersions } = readState(queryClient);
      if (dismissedVersions.includes(version)) return;
      patchState(queryClient, { dismissedVersions: [...dismissedVersions, version] });
    },
    [queryClient],
  );

  return { check, download, install, openReleasePage, dismiss };
}

/**
 * Whether a recording or a processing run is going on, so the restart has to wait. Read from the
 * existing recording status (the top bar's recording events keep it fresh) and the processing runs started here.
 */
export function useRestartBlocked(): boolean {
  const status = useRecordingStatus();
  const processing = useIsAnyProcessingRunPending();

  const recording = status.data !== undefined && status.data.state !== 'idle';
  return recording || processing;
}

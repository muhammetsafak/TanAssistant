import { useState } from 'react';
import { useMutation, useMutationState, useQuery, useQueryClient } from '@tanstack/react-query';

import {
  commands,
  events,
  type StorageUsageDto,
  type WhisperModelDownloadEvent,
  type WhisperModelDto,
} from '../../ipc/bindings';
import { unwrapSessions, unwrapTranscription } from '../../ipc/errors';
import { useTauriEvent } from '../../ipc/useTauriEvent';
import { sessionKeys } from '../sessions/keys';

export type WhisperModel = WhisperModelDto;
export type StorageUsage = StorageUsageDto;
export interface DownloadProgress {
  downloaded: number;
  total: number;
}

export const whisperKeys = {
  models: ['whisper-models'] as const,
};

export function useWhisperModels() {
  return useQuery({
    queryKey: whisperKeys.models,
    queryFn: () => unwrapTranscription(commands.listWhisperModels()),
  });
}

export function useStorageUsage() {
  return useQuery({
    queryKey: sessionKeys.storage,
    queryFn: () => unwrapSessions(commands.getStorageUsage()),
  });
}

const DOWNLOAD_KEY = ['whisper-download'] as const;

/** Downloads a model. The progress arrives separately (`useDownloadProgress`). */
export function useDownloadWhisperModel() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationKey: DOWNLOAD_KEY,
    mutationFn: (modelId: string) => unwrapTranscription(commands.downloadWhisperModel(modelId)),
    // Also on failure: an interrupted download leaves a partial file whose size the list shows.
    onSettled: async () => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: whisperKeys.models }),
        queryClient.invalidateQueries({ queryKey: sessionKeys.storage }),
      ]);
    },
  });
}

/** Ids of the models being downloaded right now, even when started before this screen opened. */
export function useDownloadingModelIds(): string[] {
  return useMutationState({
    filters: { mutationKey: DOWNLOAD_KEY, status: 'pending' },
    select: (mutation) => mutation.state.variables as string,
  });
}

/** The latest progress event of each model being downloaded. */
export function useDownloadProgress(): Record<string, DownloadProgress> {
  const [progress, setProgress] = useState<Record<string, DownloadProgress>>({});
  useTauriEvent(events.whisperModelDownloadEvent, (event: WhisperModelDownloadEvent) =>
    setProgress((current) => ({
      ...current,
      [event.modelId]: { downloaded: event.downloaded, total: event.total },
    })),
  );
  return progress;
}

export function useSelectWhisperModel() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (modelId: string) => unwrapTranscription(commands.selectWhisperModel(modelId)),
    onSettled: () => queryClient.invalidateQueries({ queryKey: whisperKeys.models }),
  });
}

export function useDeleteWhisperModel() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (modelId: string) => unwrapTranscription(commands.deleteWhisperModel(modelId)),
    onSettled: async () => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: whisperKeys.models }),
        queryClient.invalidateQueries({ queryKey: sessionKeys.storage }),
      ]);
    },
  });
}

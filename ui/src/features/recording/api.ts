import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';

import {
  commands,
  type ActiveRecordingDto,
  type AudioDeviceDto,
  type DisplayDto,
  type DisplayThumbnailDto_Serialize,
  type RecordingStatusDto_Serialize,
  type RecordingCapabilitiesDto_Serialize,
  type RecordingSummaryDto,
  type StartRecordingOptionsDto_Deserialize,
} from '../../ipc/bindings';
import { unwrapRecording } from '../../ipc/errors';
import { sessionKeys } from '../sessions/keys';

export type ActiveRecording = ActiveRecordingDto;
export type AudioDevice = AudioDeviceDto;
export type Display = DisplayDto;
export type DisplayThumbnail = DisplayThumbnailDto_Serialize;
export type RecordingCapabilities = RecordingCapabilitiesDto_Serialize;
export type RecordingSummary = RecordingSummaryDto;
export type StartOptions = StartRecordingOptionsDto_Deserialize;

export const recordingKeys = {
  status: ['recording', 'status'] as const,
  capabilities: ['recording', 'capabilities'] as const,
  audioDevices: ['recording', 'audio-devices'] as const,
  displays: ['recording', 'displays'] as const,
  thumbnail: (displayId: string) => ['recording', 'thumbnail', displayId] as const,
};

/** How often an open display picker re-reads the previews. */
export const THUMBNAIL_REFRESH_MS = 3000;

export function useRecordingStatus() {
  return useQuery({
    queryKey: recordingKeys.status,
    queryFn: () => unwrapRecording(commands.recordingStatus()),
  });
}

export function useRecordingCapabilities() {
  return useQuery({
    queryKey: recordingKeys.capabilities,
    queryFn: () => unwrapRecording(commands.recordingCapabilities()),
    staleTime: Infinity,
  });
}

/** Devices come and go (a headset is plugged in), so they are re-read every time the form opens. */
export function useAudioDevices(enabled: boolean) {
  return useQuery({
    queryKey: recordingKeys.audioDevices,
    queryFn: () => unwrapRecording(commands.listAudioDevices()),
    enabled,
    staleTime: 0,
  });
}

export function useDisplays(enabled: boolean) {
  return useQuery({
    queryKey: recordingKeys.displays,
    queryFn: () => unwrapRecording(commands.listDisplays()),
    enabled,
    staleTime: 0,
  });
}

export function useStartRecording() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (options: StartOptions) => unwrapRecording(commands.startRecording(options)),
    onSettled: async () => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: recordingKeys.status }),
        queryClient.invalidateQueries({ queryKey: sessionKeys.all }),
      ]);
    },
  });
}

export function useStopRecording() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => unwrapRecording(commands.stopRecording()),
    onSettled: async () => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: recordingKeys.status }),
        queryClient.invalidateQueries({ queryKey: sessionKeys.all }),
      ]);
    },
  });
}

/**
 * The preview of one display. It is read while the picker is open (`enabled`) and then re-read
 * every few seconds; nothing is requested while it is closed. The image shows the screen, so it
 * lives in the query cache only as long as someone is looking at it (`gcTime: 0`).
 */
export function useDisplayThumbnail(displayId: string, enabled: boolean) {
  return useQuery({
    queryKey: recordingKeys.thumbnail(displayId),
    queryFn: () => unwrapRecording(commands.displayThumbnail(displayId)),
    enabled,
    staleTime: 0,
    gcTime: 0,
    refetchInterval: (query) => {
      const reason = query.state.data?.unavailableReason;
      // These two never change while the app runs; asking again would only repeat the answer.
      return reason === 'wayland_unsupported' || reason === 'unsupported'
        ? false
        : THUMBNAIL_REFRESH_MS;
    },
    refetchIntervalInBackground: false,
  });
}

function useStatusMutation(run: () => Promise<RecordingStatusDto_Serialize>) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: run,
    onSuccess: (status) => queryClient.setQueryData(recordingKeys.status, status),
    // A failure usually means the recording changed state elsewhere (the tray): re-read it.
    onSettled: () => queryClient.invalidateQueries({ queryKey: recordingKeys.status }),
  });
}

export function usePauseRecording() {
  return useStatusMutation(() => unwrapRecording(commands.pauseRecording()));
}

export function useResumeRecording() {
  return useStatusMutation(() => unwrapRecording(commands.resumeRecording()));
}

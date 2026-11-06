import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';

import {
  commands,
  type ActiveRecordingDto,
  type AudioDeviceDto,
  type DisplayDto,
  type RecordingCapabilitiesDto_Serialize,
  type RecordingSummaryDto,
  type StartRecordingOptionsDto_Deserialize,
} from '../../ipc/bindings';
import { unwrapRecording } from '../../ipc/errors';
import { sessionKeys } from '../sessions/keys';

export type ActiveRecording = ActiveRecordingDto;
export type AudioDevice = AudioDeviceDto;
export type Display = DisplayDto;
export type RecordingCapabilities = RecordingCapabilitiesDto_Serialize;
export type RecordingSummary = RecordingSummaryDto;
export type StartOptions = StartRecordingOptionsDto_Deserialize;

export const recordingKeys = {
  status: ['recording', 'status'] as const,
  capabilities: ['recording', 'capabilities'] as const,
  audioDevices: ['recording', 'audio-devices'] as const,
  displays: ['recording', 'displays'] as const,
};

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

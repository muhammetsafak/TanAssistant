import { useCallback, useRef } from 'react';
import type { TFunction } from 'i18next';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';

import { useToast } from '../../components/ui/toast-context';
import {
  commands,
  type AiErrorCode,
  type AiIpcError,
  type ProviderKindDto,
  type ProviderKindInfoDto_Serialize,
  type ProviderProfileDto_Serialize,
  type ProviderTestReportDto_Serialize,
  type TaskAssignmentDto,
  type TaskKindDto,
  type UpsertProviderProfileDto_Deserialize,
} from '../../ipc/bindings';
import { credentialKeys } from '../credentials/api';

export type ProviderKind = ProviderKindDto;
export type TaskKind = TaskKindDto;
export type ProviderKindInfo = ProviderKindInfoDto_Serialize;
export type ProviderProfile = ProviderProfileDto_Serialize;
export type ProviderTestReport = ProviderTestReportDto_Serialize;
export type ProfileInput = UpsertProviderProfileDto_Deserialize;
export type TaskAssignment = TaskAssignmentDto;
export type { AiErrorCode };

export const TASKS: readonly TaskKind[] = ['transcription', 'vision', 'summary'];

/** Provider names are brand names; they read the same in every UI language. */
export const KIND_LABELS: Record<ProviderKind, string> = {
  openai: 'OpenAI',
  anthropic: 'Anthropic',
  gemini: 'Google Gemini',
  deepseek: 'DeepSeek',
  ollama: 'Ollama',
};

export const PROVIDER_KINDS = Object.keys(KIND_LABELS) as ProviderKind[];

export const providerKeys = {
  profiles: ['provider-profiles'] as const,
  kinds: ['provider-kinds'] as const,
  assignments: ['task-assignments'] as const,
};

/** A failure reported by the AI commands. Carries the machine-readable code; never a key. */
export class AiCommandError extends Error {
  readonly code: AiErrorCode;

  constructor(error: AiIpcError) {
    super(error.message);
    this.name = 'AiCommandError';
    this.code = error.code;
  }
}

type Result<T> = { status: 'ok'; data: T } | { status: 'error'; error: AiIpcError };

async function unwrap<T>(pending: Promise<Result<T>>): Promise<T> {
  const result = await pending;
  if (result.status === 'error') {
    throw new AiCommandError(result.error);
  }
  return result.data;
}

export interface DescribedAiError {
  /** Localised, user-facing text chosen by the error code. */
  message: string;
  /** The backend's own (English, key-free) description, when it adds information. */
  detail?: string;
}

/** Turns any thrown value into text for the user. Raw non-IPC errors are never echoed. */
export function describeAiError(error: unknown, t: TFunction): DescribedAiError {
  if (error instanceof AiCommandError) {
    const message = t(`providers.errors.${error.code}`, { defaultValue: error.message });
    return message === error.message || !error.message
      ? { message }
      : { message, detail: error.message };
  }
  return { message: t('errors.unknown') };
}

export function useProfiles() {
  return useQuery({
    queryKey: providerKeys.profiles,
    queryFn: () => unwrap(commands.listProviderProfiles()),
  });
}

/** Static facts about the provider kinds; they never change while the app runs. */
export function useProviderKinds() {
  return useQuery({
    queryKey: providerKeys.kinds,
    queryFn: () => commands.listProviderKinds(),
    staleTime: Infinity,
  });
}

export function useAssignments() {
  return useQuery({
    queryKey: providerKeys.assignments,
    queryFn: () => unwrap(commands.getTaskAssignments()),
  });
}

export function useUpsertProfile() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (input: ProfileInput) => unwrap(commands.upsertProviderProfile(input)),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: providerKeys.profiles }),
  });
}

export function useDeleteProfile() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => unwrap(commands.deleteProviderProfile(id)),
    // Deleting a profile also removes the assignments that pointed at it.
    onSuccess: () =>
      Promise.all([
        queryClient.invalidateQueries({ queryKey: providerKeys.profiles }),
        queryClient.invalidateQueries({ queryKey: providerKeys.assignments }),
      ]),
  });
}

export function useTestProvider() {
  return useMutation({
    mutationFn: (id: string) => unwrap(commands.testProvider(id)),
  });
}

/** Assigns `profileId` to `task`; `null` clears the assignment. */
export function useSetAssignment() {
  const queryClient = useQueryClient();
  // Quick successive changes (arrow keys through a select) are sent one after another, so the
  // last choice is also the last one the backend applies.
  const queue = useRef<Promise<unknown>>(Promise.resolve());
  return useMutation({
    mutationFn: ({ task, profileId }: { task: TaskKind; profileId: string | null }) => {
      const run = () =>
        profileId === null
          ? unwrap(commands.clearTaskAssignment(task))
          : unwrap(commands.setTaskAssignment(task, profileId));
      const next = queue.current.then(run, run);
      queue.current = next.catch(() => undefined);
      return next;
    },
    onSettled: () => queryClient.invalidateQueries({ queryKey: providerKeys.assignments }),
  });
}

/** The capability a task needs from a profile. */
export function capabilityFor(task: TaskKind): 'chat' | 'vision' | 'transcription' {
  return task === 'summary' ? 'chat' : task;
}

/**
 * Shows a failure as a toast. When the key could not be read from the vault, the vault status is
 * refreshed too, so the Credentials screen and this one agree on whether it is locked.
 */
export function useAiErrorHandler() {
  const { t } = useTranslation();
  const toast = useToast();
  const queryClient = useQueryClient();
  return useCallback(
    (error: unknown) => {
      const { message, detail } = describeAiError(error, t);
      toast.errorText(detail ? `${message} (${detail})` : message);
      if (error instanceof AiCommandError && error.code === 'secret_unavailable') {
        void queryClient.invalidateQueries({ queryKey: credentialKeys.vaultStatus });
      }
    },
    [t, toast, queryClient],
  );
}

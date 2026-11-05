import { useCallback } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';

import { useToast } from '../../components/ui/toast-context';
import {
  ipc,
  IpcCommandError,
  type NewSecret,
  type SecretChanges,
  type VariableInput,
} from '../../ipc/client';

export const credentialKeys = {
  variables: ['variables'] as const,
  secrets: ['secrets'] as const,
  vaultStatus: ['secret-store-status'] as const,
};

/**
 * Mutations that carry a secret value or the master password are dropped from the mutation
 * cache as soon as their form unmounts, so the plaintext does not linger in memory (the default
 * is 5 minutes).
 */
const DROP_VARIABLES_AT_ONCE = 0;

/** How long a revealed secret stays visible before it is masked again. */
export const REVEAL_DURATION_MS = 30_000;

export function useVariables() {
  return useQuery({ queryKey: credentialKeys.variables, queryFn: ipc.listVariables });
}

export function useUpsertVariable() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (input: VariableInput) => ipc.upsertVariable(input),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: credentialKeys.variables }),
  });
}

export function useDeleteVariable() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => ipc.deleteVariable(id),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: credentialKeys.variables }),
  });
}

export function useSecrets(enabled: boolean) {
  return useQuery({ queryKey: credentialKeys.secrets, queryFn: ipc.listSecrets, enabled });
}

export function useCreateSecret() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (input: NewSecret) => ipc.createSecret(input),
    gcTime: DROP_VARIABLES_AT_ONCE,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: credentialKeys.secrets }),
  });
}

export function useUpdateSecret() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (input: SecretChanges) => ipc.updateSecret(input),
    gcTime: DROP_VARIABLES_AT_ONCE,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: credentialKeys.secrets }),
  });
}

export function useDeleteSecret() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => ipc.deleteSecret(id),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: credentialKeys.secrets }),
  });
}

/**
 * Fetches one secret's value. Deliberately a plain function, not a query: the value must never
 * enter the query cache. The caller keeps it in local state only for as long as it is shown.
 */
export function revealSecret(id: string): Promise<string> {
  return ipc.revealSecret(id);
}

export function useVaultStatus() {
  return useQuery({ queryKey: credentialKeys.vaultStatus, queryFn: ipc.secretStoreStatus });
}

function useRefreshAfterUnlock() {
  const queryClient = useQueryClient();
  return useCallback(async () => {
    await queryClient.invalidateQueries({ queryKey: credentialKeys.vaultStatus });
    await queryClient.invalidateQueries({ queryKey: credentialKeys.secrets });
  }, [queryClient]);
}

export function useUnlockVault() {
  const refresh = useRefreshAfterUnlock();
  return useMutation({
    mutationFn: (password: string) => ipc.unlockSecretStore(password),
    gcTime: DROP_VARIABLES_AT_ONCE,
    onSuccess: refresh,
  });
}

export function useSetMasterPassword() {
  const refresh = useRefreshAfterUnlock();
  return useMutation({
    mutationFn: (password: string) => ipc.setMasterPassword(password, null),
    gcTime: DROP_VARIABLES_AT_ONCE,
    onSuccess: refresh,
  });
}

/**
 * Shows a failure as a toast and, when the vault turned out to be locked or unconfigured,
 * refreshes the vault status so the page offers the unlock dialog.
 */
export function useCredentialsErrorHandler() {
  const toast = useToast();
  const queryClient = useQueryClient();
  return useCallback(
    (error: unknown) => {
      toast.error(error);
      if (
        error instanceof IpcCommandError &&
        (error.code === 'secret_store_locked' || error.code === 'secret_store_not_configured')
      ) {
        void queryClient.invalidateQueries({ queryKey: credentialKeys.vaultStatus });
      }
    },
    [toast, queryClient],
  );
}

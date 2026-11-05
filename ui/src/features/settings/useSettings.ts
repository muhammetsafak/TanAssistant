import { useEffect } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';

import { ipc, type AppSettings, type SettingsPatch } from '../../ipc/client';

export const settingsQueryKey = ['settings'] as const;

export function useSettings() {
  return useQuery({ queryKey: settingsQueryKey, queryFn: ipc.getSettings });
}

export function useUpdateSettings() {
  const queryClient = useQueryClient();
  return useMutation<AppSettings, Error, SettingsPatch>({
    mutationFn: (patch) => ipc.updateSettings(patch),
    onSuccess: (settings) => {
      queryClient.setQueryData(settingsQueryKey, settings);
    },
  });
}

/** Keeps the i18n language in step with the persisted `uiLanguage` setting. */
export function useSyncUiLanguage(): void {
  const { i18n } = useTranslation();
  const { data } = useSettings();
  const persisted = data?.uiLanguage;

  useEffect(() => {
    if (persisted && persisted !== i18n.language) {
      void i18n.changeLanguage(persisted);
    }
  }, [persisted, i18n]);
}

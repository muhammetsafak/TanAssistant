import type { TFunction } from 'i18next';

import {
  commands,
  type AppSettingsDto_Serialize,
  type CreateSecretDto,
  type ErrorCode,
  type IpcError,
  type SecretDto,
  type SecretStoreStatusDto,
  type SettingsPatchDto_Deserialize,
  type UpdateSecretDto_Deserialize,
  type UpsertVariableDto_Deserialize,
  type VariableDto,
} from './bindings';

export type AppSettings = AppSettingsDto_Serialize;
export type SettingsPatch = SettingsPatchDto_Deserialize;
export type NewSecret = CreateSecretDto;
export type SecretChanges = UpdateSecretDto_Deserialize;
export type VariableInput = UpsertVariableDto_Deserialize;
export type { ErrorCode, SecretDto, SecretStoreStatusDto, VariableDto };

/** A failure reported by the backend. Carries the machine-readable code; never a secret value. */
export class IpcCommandError extends Error {
  readonly code: ErrorCode;

  constructor(error: IpcError) {
    super(error.message);
    this.name = 'IpcCommandError';
    this.code = error.code;
  }
}

type Result<T> = { status: 'ok'; data: T } | { status: 'error'; error: IpcError };

async function unwrap<T>(pending: Promise<Result<T>>): Promise<T> {
  const result = await pending;
  if (result.status === 'error') {
    throw new IpcCommandError(result.error);
  }
  return result.data;
}

/**
 * Typed IPC client: the only place the UI talks to the backend. Every call resolves with the
 * data or rejects with an `IpcCommandError`.
 */
export const ipc = {
  appVersion: (): Promise<string> => commands.appVersion(),

  listVariables: () => unwrap(commands.listVariables()),
  upsertVariable: (input: VariableInput) => unwrap(commands.upsertVariable(input)),
  deleteVariable: (id: string) => unwrap(commands.deleteVariable(id)),

  listSecrets: () => unwrap(commands.listSecrets()),
  createSecret: (input: NewSecret) => unwrap(commands.createSecret(input)),
  updateSecret: (input: SecretChanges) => unwrap(commands.updateSecret(input)),
  deleteSecret: (id: string) => unwrap(commands.deleteSecret(id)),
  revealSecret: (id: string) => unwrap(commands.revealSecret(id)),

  secretStoreStatus: (): Promise<SecretStoreStatusDto> => commands.secretStoreStatus(),
  unlockSecretStore: (password: string) => unwrap(commands.unlockSecretStore(password)),
  setMasterPassword: (newPassword: string, currentPassword: string | null = null) =>
    unwrap(commands.setMasterPassword(newPassword, currentPassword)),

  getSettings: () => unwrap(commands.getSettings()),
  updateSettings: (patch: SettingsPatch) => unwrap(commands.updateSettings(patch)),
};

export interface DescribedError {
  /** Localised, user-facing text chosen by the error code. */
  message: string;
  /** The backend's own (English, value-free) description, when it adds information. */
  detail?: string;
}

/** Turns any thrown value into text for the user. Raw non-IPC errors are never echoed. */
export function describeError(error: unknown, t: TFunction): DescribedError {
  if (error instanceof IpcCommandError) {
    const message = t(`errors.${error.code}`, { defaultValue: error.message });
    return message === error.message ? { message } : { message, detail: error.message };
  }
  return { message: t('errors.unknown') };
}

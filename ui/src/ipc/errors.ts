import { useCallback } from 'react';
import type { TFunction } from 'i18next';
import { useTranslation } from 'react-i18next';

import { useToast } from '../components/ui/toast-context';
import type {
  ExportProblemCode,
  ProcessingErrorCode,
  ProcessingIpcError_Serialize,
  RecordingErrorCode,
  RecordingIpcError,
  SessionsErrorCode,
  SessionsIpcError,
  TranscriptionErrorCode,
  TranscriptionIpcError,
} from './bindings';
import { describeError, type DescribedError } from './client';

/** A failure of a recording command. Carries the machine-readable code. */
export class RecordingCommandError extends Error {
  readonly code: RecordingErrorCode;

  constructor(error: RecordingIpcError) {
    super(error.message);
    this.name = 'RecordingCommandError';
    this.code = error.code;
  }
}

/** A failure of a session command (list, search, detail, export, delete...). */
export class SessionsCommandError extends Error {
  readonly code: SessionsErrorCode;
  /** Why an export destination was refused (with `code: validation`); never parse the message. */
  readonly exportProblem: ExportProblemCode | null;

  constructor(error: SessionsIpcError) {
    super(error.message);
    this.name = 'SessionsCommandError';
    this.code = error.code;
    this.exportProblem = error.exportProblem ?? null;
  }
}

/** A failure of a processing command; `transcriptionCode` explains `transcription_failed`. */
export class ProcessingCommandError extends Error {
  readonly code: ProcessingErrorCode;
  readonly transcriptionCode: TranscriptionErrorCode | null;

  constructor(error: ProcessingIpcError_Serialize) {
    super(error.message);
    this.name = 'ProcessingCommandError';
    this.code = error.code;
    this.transcriptionCode = error.transcriptionCode;
  }
}

/** A failure of a transcription command (whisper models, estimate). */
export class TranscriptionCommandError extends Error {
  readonly code: TranscriptionErrorCode;

  constructor(error: TranscriptionIpcError) {
    super(error.message);
    this.name = 'TranscriptionCommandError';
    this.code = error.code;
  }
}

type Result<T, E> = { status: 'ok'; data: T } | { status: 'error'; error: E };

async function settle<T, E>(pending: Promise<Result<T, E>>, wrap: (error: E) => Error): Promise<T> {
  const result = await pending;
  if (result.status === 'error') {
    throw wrap(result.error);
  }
  return result.data;
}

export const unwrapRecording = <T>(pending: Promise<Result<T, RecordingIpcError>>) =>
  settle(pending, (error) => new RecordingCommandError(error));

export const unwrapSessions = <T>(pending: Promise<Result<T, SessionsIpcError>>) =>
  settle(pending, (error) => new SessionsCommandError(error));

export const unwrapProcessing = <T>(pending: Promise<Result<T, ProcessingIpcError_Serialize>>) =>
  settle(pending, (error) => new ProcessingCommandError(error));

export const unwrapTranscription = <T>(pending: Promise<Result<T, TranscriptionIpcError>>) =>
  settle(pending, (error) => new TranscriptionCommandError(error));

function withDetail(message: string, raw: string): DescribedError {
  return message === raw || !raw ? { message } : { message, detail: raw };
}

/**
 * Turns a thrown value of any of the commands above into text for the user, chosen by its code.
 * The backend's own English text is only added as a detail; other values are never echoed.
 */
export function describeCommandError(error: unknown, t: TFunction): DescribedError {
  if (error instanceof RecordingCommandError) {
    return withDetail(
      t(`recording.errors.${error.code}`, { defaultValue: error.message }),
      error.message,
    );
  }
  if (error instanceof SessionsCommandError) {
    return withDetail(
      t(`sessions.errors.${error.code}`, { defaultValue: error.message }),
      error.message,
    );
  }
  if (error instanceof ProcessingCommandError) {
    if (error.code === 'transcription_failed' && error.transcriptionCode) {
      return withDetail(
        t(`transcription.errors.${error.transcriptionCode}`, { defaultValue: error.message }),
        error.message,
      );
    }
    return withDetail(
      t(`processing.errors.${error.code}`, { defaultValue: error.message }),
      error.message,
    );
  }
  if (error instanceof TranscriptionCommandError) {
    return withDetail(
      t(`transcription.errors.${error.code}`, { defaultValue: error.message }),
      error.message,
    );
  }
  return describeError(error, t);
}

/** Returns a function that shows any command failure as an error toast. */
export function useCommandErrorHandler() {
  const { t } = useTranslation();
  const toast = useToast();
  return useCallback(
    (error: unknown) => {
      const { message, detail } = describeCommandError(error, t);
      toast.errorText(detail ? `${message} (${detail})` : message);
    },
    [t, toast],
  );
}

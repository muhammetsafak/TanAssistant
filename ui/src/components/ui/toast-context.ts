import { createContext, useContext } from 'react';

export interface ToastApi {
  success: (message: string) => void;
  /** A calm, already-localised notice: neither a success nor an error. */
  info: (message: string) => void;
  /** Accepts any thrown value; IPC errors are localised by their code. */
  error: (error: unknown) => void;
  /** A plain, already-localised error text (no thrown value involved). */
  errorText: (message: string) => void;
}

export const ToastContext = createContext<ToastApi | null>(null);

export function useToast(): ToastApi {
  const value = useContext(ToastContext);
  if (!value) {
    throw new Error('useToast must be used inside <ToastProvider>');
  }
  return value;
}

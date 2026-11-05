import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';

import { describeError } from '../../ipc/client';
import { Button } from './Button';
import { ToastContext, type ToastApi } from './toast-context';

import './ui.css';

type ToastKind = 'success' | 'error';

interface ToastEntry {
  id: number;
  kind: ToastKind;
  message: string;
  detail?: string;
}

const SUCCESS_MS = 4000;
const ERROR_MS = 10000;

function ToastItem({ toast, onDismiss }: { toast: ToastEntry; onDismiss: (id: number) => void }) {
  const { t } = useTranslation();

  useEffect(() => {
    const timer = setTimeout(
      () => onDismiss(toast.id),
      toast.kind === 'error' ? ERROR_MS : SUCCESS_MS,
    );
    return () => clearTimeout(timer);
  }, [toast.id, toast.kind, onDismiss]);

  return (
    <div
      className={`toast toast--${toast.kind}`}
      role={toast.kind === 'error' ? 'alert' : 'status'}
    >
      <p className="toast__text">
        {toast.message}
        {toast.detail ? <span className="toast__detail">{toast.detail}</span> : null}
      </p>
      <Button size="sm" onClick={() => onDismiss(toast.id)} aria-label={t('toast.dismiss')}>
        {t('common.close')}
      </Button>
    </div>
  );
}

/** Renders toasts into <body> (outside #root) so they stay readable while a dialog is open. */
export function ToastProvider({ children }: { children: ReactNode }) {
  const { t } = useTranslation();
  const [toasts, setToasts] = useState<ToastEntry[]>([]);
  const nextId = useRef(1);

  const dismiss = useCallback((id: number) => {
    setToasts((current) => current.filter((entry) => entry.id !== id));
  }, []);

  const push = useCallback((entry: Omit<ToastEntry, 'id'>) => {
    const id = nextId.current++;
    setToasts((current) => [...current, { ...entry, id }]);
  }, []);

  const api = useMemo<ToastApi>(
    () => ({
      success: (message) => push({ kind: 'success', message }),
      error: (error) => push({ kind: 'error', ...describeError(error, t) }),
      errorText: (message) => push({ kind: 'error', message }),
    }),
    [push, t],
  );

  return (
    <ToastContext.Provider value={api}>
      {children}
      {createPortal(
        <div className="toast-region">
          {toasts.map((entry) => (
            <ToastItem key={entry.id} toast={entry} onDismiss={dismiss} />
          ))}
        </div>,
        document.body,
      )}
    </ToastContext.Provider>
  );
}

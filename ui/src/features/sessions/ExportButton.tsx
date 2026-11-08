import { useState } from 'react';
import { save } from '@tauri-apps/plugin-dialog';
import { useTranslation } from 'react-i18next';

import { Button } from '../../components/ui/Button';
import { useToast } from '../../components/ui/toast-context';
import { SessionsCommandError, useCommandErrorHandler } from '../../ipc/errors';
import { useExportSession } from './api';
import { suggestedFileName } from './format';

import '../../components/ui/ui.css';
import './sessions.css';

/**
 * Exports the note, key frames and transcript as a Markdown file at a place the user picks in the
 * native save dialog. The path the dialog returns is sent as it is: the backend adds the `.md` extension when it is
 * missing and checks the place. The backend copies the key frames into a `<name>_frames` folder beside it;
 * the written path stays on screen so it can be found and copied.
 */
export function ExportButton({
  sessionId,
  title,
  disabled,
}: {
  sessionId: string;
  title: string;
  disabled?: boolean;
}) {
  const { t } = useTranslation();
  const toast = useToast();
  const handleError = useCommandErrorHandler();
  const exportSession = useExportSession();
  const [choosing, setChoosing] = useState(false);
  const [exportedPath, setExportedPath] = useState<string | null>(null);

  async function startExport() {
    setChoosing(true);
    let destination: string | null;
    try {
      destination = await save({
        title: t('sessions.export.dialogTitle'),
        defaultPath: suggestedFileName(title),
        filters: [{ name: 'Markdown', extensions: ['md'] }],
      });
    } catch (error) {
      handleError(error);
      return;
    } finally {
      setChoosing(false);
    }
    // Cancelling the dialog is a choice, not a failure.
    if (destination === null) return;

    exportSession.mutate(
      { sessionId, destination },
      {
        onSuccess: (path) => {
          setExportedPath(path);
          toast.success(t('sessions.export.done'));
        },
        onError: handleExportError,
      },
    );
  }

  // The backend names what is wrong with the chosen place; say what to do about it.
  function handleExportError(error: unknown) {
    if (error instanceof SessionsCommandError && error.code === 'validation') {
      // An unknown reason (a newer backend) falls back to the general text.
      toast.errorText(
        error.exportProblem
          ? t(`sessions.export.problems.${error.exportProblem}`, {
              defaultValue: t('sessions.export.errors.validation'),
            })
          : t('sessions.export.errors.validation'),
      );
    } else if (error instanceof SessionsCommandError && error.code === 'files_failed') {
      toast.errorText(t('sessions.export.errors.filesFailed'));
    } else {
      handleError(error);
    }
  }

  async function copyPath() {
    if (!exportedPath) return;
    try {
      await navigator.clipboard.writeText(exportedPath);
      toast.success(t('sessions.export.pathCopied'));
    } catch {
      toast.errorText(t('errors.clipboard'));
    }
  }

  return (
    <div className="export">
      <Button
        variant="primary"
        onClick={() => void startExport()}
        disabled={disabled || choosing || exportSession.isPending}
      >
        {exportSession.isPending ? t('sessions.export.exporting') : t('sessions.export.button')}
      </Button>
      {exportedPath ? (
        <div className="export__result" role="status">
          <p>{t('sessions.export.savedTo')}</p>
          <code className="export__path">{exportedPath}</code>
          <p>{t('sessions.export.framesNote')}</p>
          <div className="export__actions">
            <Button size="sm" onClick={() => void copyPath()}>
              {t('sessions.export.copyPath')}
            </Button>
            <Button size="sm" onClick={() => setExportedPath(null)}>
              {t('common.close')}
            </Button>
          </div>
        </div>
      ) : null}
    </div>
  );
}

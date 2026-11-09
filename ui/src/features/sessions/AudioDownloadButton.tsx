import { useState } from 'react';
import { save } from '@tauri-apps/plugin-dialog';
import { useTranslation } from 'react-i18next';

import { Button } from '../../components/ui/Button';
import { useToast } from '../../components/ui/toast-context';
import { SessionsCommandError, useCommandErrorHandler } from '../../ipc/errors';
import { useExportSessionAudio } from './api';
import { suggestedFileName } from './format';

import '../../components/ui/ui.css';
import './sessions.css';

/**
 * Saves the session's audio as one mixed WAV file at a place the user picks in the native save
 * dialog. The path the dialog returns is sent as it is: the backend adds the `.wav` extension when
 * it is missing and checks the place.
 */
export function AudioDownloadButton({ sessionId, title }: { sessionId: string; title: string }) {
  const { t } = useTranslation();
  const toast = useToast();
  const handleError = useCommandErrorHandler();
  const exportAudio = useExportSessionAudio();
  const [choosing, setChoosing] = useState(false);

  async function startDownload() {
    setChoosing(true);
    let destination: string | null;
    try {
      destination = await save({
        title: t('sessions.audio.download.dialogTitle'),
        defaultPath: suggestedFileName(title, 'wav', t('sessions.audio.download.defaultName')),
        filters: [{ name: 'WAV', extensions: ['wav'] }],
      });
    } catch (error) {
      handleError(error);
      return;
    } finally {
      setChoosing(false);
    }
    // Cancelling the dialog is a choice, not a failure.
    if (destination === null) return;

    exportAudio.mutate(
      { sessionId, destination },
      {
        onSuccess: () => toast.success(t('sessions.audio.download.done')),
        onError: handleDownloadError,
      },
    );
  }

  // The backend names what is wrong; say what to do about it. The WAV wording wins over the
  // Markdown export's where both exist, and an unknown reason (a newer backend) gets the general text.
  function handleDownloadError(error: unknown) {
    if (error instanceof SessionsCommandError && error.code === 'validation') {
      const general = t('sessions.audio.download.errors.validation');
      toast.errorText(
        error.exportProblem
          ? t(
              [
                `sessions.audio.download.problems.${error.exportProblem}`,
                `sessions.export.problems.${error.exportProblem}`,
              ],
              { defaultValue: general },
            )
          : general,
      );
    } else if (error instanceof SessionsCommandError && error.code === 'files_failed') {
      toast.errorText(t('sessions.audio.download.errors.filesFailed'));
    } else {
      handleError(error);
    }
  }

  const pending = choosing || exportAudio.isPending;
  return (
    <Button
      size="sm"
      variant="primary"
      onClick={() => void startDownload()}
      disabled={pending}
      aria-busy={exportAudio.isPending}
    >
      {exportAudio.isPending
        ? t('sessions.audio.download.preparing')
        : t('sessions.audio.download.button')}
    </Button>
  );
}

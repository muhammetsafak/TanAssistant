import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { save } from '@tauri-apps/plugin-dialog';
import { Route, Routes } from 'react-router';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '../../i18n';
import { commands } from '../../ipc/bindings';
import SessionDetailPage from '../../pages/SessionDetailPage';
import { emitEvent } from '../../test-events';
import { fail, ok, renderWithProviders } from '../../test-utils';
import { session, sessionDetail } from './fixtures';

vi.mock('@tauri-apps/plugin-dialog', () => ({ save: vi.fn() }));

vi.mock('../../ipc/bindings', async () => {
  const { stubEvents } = await import('../../test-events');
  return {
    commands: {
      getSessionDetail: vi.fn(),
      toggleActionItem: vi.fn(),
      processingStatus: vi.fn(),
      processSession: vi.fn(),
      setTaskAssignment: vi.fn(),
      retryProcessing: vi.fn(),
      cancelProcessing: vi.fn(),
      estimateTranscription: vi.fn(),
      listProviderProfiles: vi.fn(),
      deleteSession: vi.fn(),
      exportSessionMarkdown: vi.fn(),
    },
    events: stubEvents(),
  };
});

function renderDetail() {
  return renderWithProviders(
    <Routes>
      <Route path="recordings/:sessionId" element={<SessionDetailPage />} />
      <Route path="recordings" element={<p>library</p>} />
    </Routes>,
    '/recordings/s1',
  );
}

describe('SessionDetailPage', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
    vi.mocked(commands.getSessionDetail).mockResolvedValue(ok(sessionDetail()));
    vi.mocked(commands.processingStatus).mockResolvedValue(
      ok({ running: false, job: sessionDetail().job }),
    );
    vi.mocked(commands.listProviderProfiles).mockResolvedValue(ok([]));
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  describe('action items', () => {
    it('calls toggle_action_item and ticks the box before the call returns', async () => {
      const user = userEvent.setup();
      let finish: (value: unknown) => void = () => undefined;
      vi.mocked(commands.toggleActionItem).mockReturnValue(
        new Promise((resolve) => {
          finish = resolve;
        }) as never,
      );
      renderDetail();

      await user.click(await screen.findByRole('tab', { name: /Aksiyonlar/ }));
      const box = await screen.findByRole('checkbox', { name: 'Bütçeyi paylaş' });
      expect(box).not.toBeChecked();

      await user.click(box);

      expect(commands.toggleActionItem).toHaveBeenCalledWith('a1', true);
      // The call is still pending: the tick is the optimistic one.
      expect(screen.getByRole('checkbox', { name: 'Bütçeyi paylaş' })).toBeChecked();
      expect(screen.getByText('2/2 tamamlandı')).toBeInTheDocument();

      finish(ok(null));
      await waitFor(() => expect(commands.getSessionDetail).toHaveBeenCalledTimes(2));
    });

    it('puts the tick back and reports when the backend refuses', async () => {
      const user = userEvent.setup();
      vi.mocked(commands.toggleActionItem).mockResolvedValue(fail('not_found') as never);
      renderDetail();

      await user.click(await screen.findByRole('tab', { name: /Aksiyonlar/ }));
      await user.click(await screen.findByRole('checkbox', { name: 'Bütçeyi paylaş' }));

      expect(await screen.findByRole('alert')).toHaveTextContent('Kayıt bulunamadı.');
      await waitFor(() =>
        expect(screen.getByRole('checkbox', { name: 'Bütçeyi paylaş' })).not.toBeChecked(),
      );
    });
  });

  describe('transcript', () => {
    it('shows the frame closest in time to the clicked line', async () => {
      const user = userEvent.setup();
      renderDetail();

      await user.click(await screen.findByRole('tab', { name: /Transkript/ }));
      const preview = screen.getByRole('complementary', { name: /en yakın kare/ });
      expect(preview).toHaveTextContent('Bir satırın zamanına tıklayın');

      // 01:10 is 5 s from the 01:05 frame (Q4 budget) and 60 s from the 00:10 one.
      await user.click(screen.getByText('Bütçeye bakalım'));

      expect(preview).toHaveTextContent('01:10 anına en yakın kare: 01:05');
      expect(preview).toHaveTextContent('Slide: Q4 budget');
      expect(preview).not.toHaveTextContent('Q3 plan');

      // 00:12 is closest to the 00:10 frame; the time button does the same by keyboard.
      await user.click(screen.getByRole('button', { name: '00:12 anına en yakın kareyi göster' }));
      expect(preview).toHaveTextContent('Slide: Q3 plan');
    });

    it('labels the speakers of the two tracks', async () => {
      const user = userEvent.setup();
      renderDetail();

      await user.click(await screen.findByRole('tab', { name: /Transkript/ }));
      const lines = within(screen.getByRole('list', { name: 'Transkript satırları' }));
      expect(lines.getByText('Ben')).toBeInTheDocument();
      expect(lines.getByText('Diğerleri')).toBeInTheDocument();
    });
  });

  describe('processing', () => {
    it('offers "Tekrar dene" when a step failed, and retries from there', async () => {
      const user = userEvent.setup();
      const failedJob = {
        ...sessionDetail().job!,
        steps: [
          { step: 'transcribe' as const, state: 'done' as const, error: null },
          { step: 'describe_frames' as const, state: 'skipped' as const, error: null },
          { step: 'synthesize' as const, state: 'failed' as const, error: 'model said no' },
        ],
        finishedAt: null,
      };
      vi.mocked(commands.getSessionDetail).mockResolvedValue(
        ok(
          sessionDetail({ session: { ...session, status: 'failed' }, note: null, job: failedJob }),
        ),
      );
      vi.mocked(commands.processingStatus).mockResolvedValue(
        ok({ running: false, job: failedJob }),
      );
      vi.mocked(commands.retryProcessing).mockResolvedValue(new Promise(() => undefined) as never);
      renderDetail();

      expect(await screen.findByText('İşleme tamamlanamadı')).toBeInTheDocument();
      expect(screen.getByText('model said no')).toBeInTheDocument();
      await user.click(screen.getByRole('button', { name: 'Tekrar dene' }));

      expect(commands.retryProcessing).toHaveBeenCalledWith('s1', null);
    });

    it('does not carry the previous run progress into a retry', async () => {
      const user = userEvent.setup();
      const failedJob = {
        ...sessionDetail().job!,
        steps: [
          { step: 'transcribe' as const, state: 'done' as const, error: null },
          { step: 'describe_frames' as const, state: 'skipped' as const, error: null },
          { step: 'synthesize' as const, state: 'failed' as const, error: 'model said no' },
        ],
        finishedAt: null,
      };
      vi.mocked(commands.getSessionDetail).mockResolvedValue(
        ok(
          sessionDetail({ session: { ...session, status: 'failed' }, note: null, job: failedJob }),
        ),
      );
      vi.mocked(commands.processingStatus).mockResolvedValue(
        ok({ running: false, job: failedJob }),
      );
      let failRun: (value: unknown) => void = () => undefined;
      vi.mocked(commands.retryProcessing)
        .mockReturnValueOnce(
          new Promise((resolve) => {
            failRun = resolve;
          }) as never,
        )
        .mockReturnValue(new Promise(() => undefined) as never);
      renderDetail();

      await user.click(await screen.findByRole('button', { name: 'Tekrar dene' }));
      emitEvent('processingProgressEvent', {
        sessionId: 's1',
        step: 'synthesize',
        stepPercent: 100,
        percent: 100,
      });
      expect(await screen.findByText(/%100/)).toBeInTheDocument();

      failRun({
        status: 'error',
        error: { code: 'internal', transcriptionCode: null, message: 'again' },
      });
      await user.click(await screen.findByRole('button', { name: 'Tekrar dene' }));

      expect(await screen.findByText(/Başlatılıyor/)).toBeInTheDocument();
      expect(screen.queryByText(/%100/)).not.toBeInTheDocument();
    });

    it('explains a missing summary provider and keeps "Tekrar dene" at hand', async () => {
      const user = userEvent.setup();
      vi.mocked(commands.getSessionDetail).mockResolvedValue(
        ok(sessionDetail({ session: { ...session, status: 'recorded' }, note: null, job: null })),
      );
      vi.mocked(commands.processingStatus).mockResolvedValue(ok({ running: false, job: null }));
      vi.mocked(commands.estimateTranscription).mockResolvedValue(
        ok({
          engine: 'local_whisper',
          providerKind: null,
          estimatedSeconds: 120,
          backend: 'vulkan (Test GPU)',
          audioSeconds: 600,
          speechSeconds: 400,
        }),
      );
      vi.mocked(commands.processSession).mockResolvedValue({
        status: 'error',
        error: { code: 'no_chat_model', transcriptionCode: null, message: 'no model' },
      } as never);
      renderDetail();

      expect(
        await screen.findByText(/Yerel transkripsiyon tahmini: yaklaşık 2 dk/),
      ).toBeInTheDocument();
      await user.click(screen.getByRole('button', { name: 'İşle' }));

      expect(await screen.findByText(/Özet için atanmış bir AI sağlayıcı yok/)).toBeInTheDocument();
      expect(screen.getByRole('button', { name: 'Tekrar dene' })).toBeInTheDocument();
      expect(screen.getByRole('link', { name: 'AI Sağlayıcıları sayfasına git' })).toHaveAttribute(
        'href',
        '/providers',
      );
    });

    it('suggests a cloud profile when local transcription is long and CPU-only', async () => {
      const user = userEvent.setup();
      vi.mocked(commands.processSession).mockReturnValue(new Promise(() => undefined) as never);
      vi.mocked(commands.getSessionDetail).mockResolvedValue(
        ok(sessionDetail({ session: { ...session, status: 'recorded' }, note: null, job: null })),
      );
      vi.mocked(commands.processingStatus).mockResolvedValue(ok({ running: false, job: null }));
      vi.mocked(commands.estimateTranscription).mockResolvedValue(
        ok({
          engine: 'local_whisper',
          providerKind: null,
          estimatedSeconds: 3 * 3600,
          backend: 'cpu',
          audioSeconds: 3600,
          speechSeconds: 3000,
        }),
      );
      vi.mocked(commands.listProviderProfiles).mockResolvedValue(
        ok([
          {
            id: 'p1',
            kind: 'openai',
            displayName: 'OpenAI work',
            model: 'gpt-4o-mini-transcribe',
            baseUrl: null,
            secretRef: 's1',
            supportsVision: true,
            capabilities: { chat: true, vision: true, transcription: true },
          },
        ]),
      );
      renderDetail();

      expect(
        await screen.findByText(/Yerel transkripsiyon tahmini: yaklaşık 3 sa/),
      ).toBeInTheDocument();
      expect(await screen.findByRole('button', { name: 'Bulutla işle' })).toBeInTheDocument();
      expect(screen.getByRole('button', { name: 'Yerelde işle' })).toBeInTheDocument();
      expect(screen.getByText('Profil: OpenAI work')).toBeInTheDocument();

      // The cloud profile is for this run only: the general assignment is never touched.
      await user.click(screen.getByRole('button', { name: 'Bulutla işle' }));
      expect(commands.processSession).toHaveBeenCalledWith('s1', 'p1');
      expect(commands.setTaskAssignment).not.toHaveBeenCalled();
    });
  });

  describe('header', () => {
    it('asks before deleting and leaves for the library afterwards', async () => {
      const user = userEvent.setup();
      vi.mocked(commands.deleteSession).mockResolvedValue(ok(null));
      renderDetail();

      await user.click(await screen.findByRole('button', { name: 'Kaydı sil' }));
      expect(commands.deleteSession).not.toHaveBeenCalled();
      await user.click(
        within(screen.getByRole('dialog')).getByRole('button', { name: 'Kaydı sil' }),
      );

      expect(commands.deleteSession).toHaveBeenCalledWith('s1', true);
      expect(await screen.findByText('library')).toBeInTheDocument();
    });

    it('exports to the file chosen in the save dialog and shows where it went', async () => {
      const user = userEvent.setup();
      vi.mocked(save).mockResolvedValue('/notes/weekly.md');
      vi.mocked(commands.exportSessionMarkdown).mockResolvedValue(ok('/notes/weekly.md'));
      renderDetail();

      await user.click(await screen.findByRole('button', { name: 'Markdown olarak dışa aktar' }));

      expect(save).toHaveBeenCalledWith(
        expect.objectContaining({
          defaultPath: 'Weekly sync.md',
          filters: [{ name: 'Markdown', extensions: ['md'] }],
        }),
      );
      expect(commands.exportSessionMarkdown).toHaveBeenCalledWith('s1', '/notes/weekly.md');
      expect(await screen.findByText('/notes/weekly.md')).toBeInTheDocument();
      expect(screen.getByText(/_frames/)).toBeInTheDocument();
    });

    it('sends the path from the dialog as it is and shows the path the backend wrote', async () => {
      const user = userEvent.setup();
      vi.mocked(save).mockResolvedValue('/notes/weekly');
      vi.mocked(commands.exportSessionMarkdown).mockResolvedValue(ok('/notes/weekly.md'));
      renderDetail();

      await user.click(await screen.findByRole('button', { name: 'Markdown olarak dışa aktar' }));

      // The backend adds the extension; the UI does not touch the chosen path.
      await waitFor(() =>
        expect(commands.exportSessionMarkdown).toHaveBeenCalledWith('s1', '/notes/weekly'),
      );
      expect(await screen.findByText('/notes/weekly.md')).toBeInTheDocument();
    });

    it('tells the user what to do when the backend refuses the chosen place', async () => {
      const user = userEvent.setup();
      vi.mocked(save).mockResolvedValue('/elsewhere/weekly.md');
      vi.mocked(commands.exportSessionMarkdown).mockResolvedValue(fail('validation') as never);
      renderDetail();

      await user.click(await screen.findByRole('button', { name: 'Markdown olarak dışa aktar' }));

      expect(await screen.findByText(/Seçilen konuma yazılamaz/)).toBeInTheDocument();
      expect(screen.queryByText('Girilen bilgiler geçersiz.')).not.toBeInTheDocument();
    });

    describe.each([
      ['not_absolute', /tam bir dosya yolu değil/],
      ['parent_reference', /Yol “\.\.” içeriyor/],
      ['network_path', /Ağ ve aygıt yollarına yazılamaz/],
      ['missing_name', /Bir dosya adı girin/],
      ['invalid_extension', /uzantısı \.md olmalı/],
      ['not_dialog_selected', /penceresinden seçilmedi/],
      ['inside_data_dir', /veri klasörüne dışa aktarılamaz/],
      ['folder_missing', /Seçilen klasör yok/],
      ['already_exists', /bu adda bir dosya zaten var/],
    ])('export problem %s', (exportProblem, message) => {
      it('shows its own explanation', async () => {
        const user = userEvent.setup();
        vi.mocked(save).mockResolvedValue('/notes/weekly.md');
        vi.mocked(commands.exportSessionMarkdown).mockResolvedValue({
          status: 'error',
          error: { code: 'validation', message: 'export destination: raw', exportProblem },
        } as never);
        renderDetail();

        await user.click(await screen.findByRole('button', { name: 'Markdown olarak dışa aktar' }));

        expect(await screen.findByText(message)).toBeInTheDocument();
        expect(screen.queryByText(/Seçilen konuma yazılamaz/)).not.toBeInTheDocument();
      });
    });

    it('falls back to the general text for a reason it does not know', async () => {
      const user = userEvent.setup();
      vi.mocked(save).mockResolvedValue('/notes/weekly.md');
      vi.mocked(commands.exportSessionMarkdown).mockResolvedValue({
        status: 'error',
        error: { code: 'validation', message: 'raw', exportProblem: 'something_new' },
      } as never);
      renderDetail();

      await user.click(await screen.findByRole('button', { name: 'Markdown olarak dışa aktar' }));

      expect(await screen.findByText(/Seçilen konuma yazılamaz/)).toBeInTheDocument();
    });

    it('explains a failed file write', async () => {
      const user = userEvent.setup();
      vi.mocked(save).mockResolvedValue('/notes/weekly.md');
      vi.mocked(commands.exportSessionMarkdown).mockResolvedValue(fail('files_failed') as never);
      renderDetail();

      await user.click(await screen.findByRole('button', { name: 'Markdown olarak dışa aktar' }));

      expect(await screen.findByText(/Dosya yazılamadı/)).toBeInTheDocument();
    });

    it('does nothing when the save dialog is cancelled', async () => {
      const user = userEvent.setup();
      vi.mocked(save).mockResolvedValue(null);
      renderDetail();

      await user.click(await screen.findByRole('button', { name: 'Markdown olarak dışa aktar' }));

      await waitFor(() => expect(save).toHaveBeenCalled());
      expect(commands.exportSessionMarkdown).not.toHaveBeenCalled();
      expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    });
  });
});

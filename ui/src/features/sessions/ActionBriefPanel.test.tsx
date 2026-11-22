import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '../../i18n';
import { commands } from '../../ipc/bindings';
import { fail, ok, renderWithProviders } from '../../test-utils';
import { ActionBriefPanel, type ActionBriefPanelProps } from './ActionBriefPanel';
import { actionBrief, frames, sessionDetail } from './fixtures';

// Without the Tauri runtime the asset URL cannot be built; the frame thumbnails need one.
vi.mock('@tauri-apps/api/core', () => ({
  convertFileSrc: (path: string) => `asset://localhost/${encodeURIComponent(path)}`,
}));

vi.mock('../../ipc/bindings', () => ({
  commands: {
    getActionBrief: vi.fn(),
    generateActionBrief: vi.fn(),
    exportActionBriefMarkdown: vi.fn(),
  },
}));

const item = sessionDetail().note!.actionItems[0]!;

function renderPanel(overrides: Partial<ActionBriefPanelProps> = {}) {
  const onClose = vi.fn();
  const onSeek = vi.fn();
  renderWithProviders(
    <ActionBriefPanel
      item={item}
      sessionDir={String.raw`C:\data\sessions\s1`}
      frames={frames}
      onSeek={onSeek}
      onClose={onClose}
      {...overrides}
    />,
  );
  return { onClose, onSeek };
}

describe('ActionBriefPanel', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
    vi.mocked(commands.getActionBrief).mockReset().mockResolvedValue(ok(actionBrief));
    vi.mocked(commands.generateActionBrief).mockReset();
    vi.mocked(commands.exportActionBriefMarkdown).mockReset();
  });

  it('is a dialog named after the brief and describing the task', async () => {
    renderPanel();

    const dialog = await screen.findByRole('dialog', { name: 'Aksiyon özeti' });
    expect(dialog).toHaveAccessibleDescription('Bütçeyi paylaş');
    expect(dialog).toHaveClass('dialog--drawer');
  });

  it('offers to generate when there is no brief and asks the backend for it', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.getActionBrief).mockResolvedValue(ok(null));
    vi.mocked(commands.generateActionBrief).mockResolvedValue(ok(actionBrief));
    renderPanel();

    expect(await screen.findByText('Bu aksiyon için henüz özet yok.')).toBeInTheDocument();
    expect(
      screen.queryByRole('button', { name: 'Markdown olarak kopyala' }),
    ).not.toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Özet oluştur' }));

    expect(commands.generateActionBrief).toHaveBeenCalledWith('a1');
    expect(await screen.findByText(actionBrief.context)).toBeInTheDocument();
    expect(screen.queryByText('Bu aksiyon için henüz özet yok.')).not.toBeInTheDocument();
  });

  it('shows progress while the brief is being generated', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.getActionBrief).mockResolvedValue(ok(null));
    let finish: (value: ReturnType<typeof ok>) => void = () => undefined;
    vi.mocked(commands.generateActionBrief).mockReturnValue(
      new Promise((resolve) => {
        finish = resolve as typeof finish;
      }),
    );
    renderPanel();

    await user.click(await screen.findByRole('button', { name: 'Özet oluştur' }));

    expect(await screen.findByText('Özet hazırlanıyor…', { selector: 'p' })).toBeInTheDocument();
    expect(screen.getByRole('progressbar', { name: 'Özet hazırlanıyor…' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Özet oluştur' })).not.toBeInTheDocument();

    finish(ok(actionBrief));
    expect(await screen.findByText(actionBrief.context)).toBeInTheDocument();
    expect(screen.queryByRole('progressbar')).not.toBeInTheDocument();
  });

  it('renders every section of the brief', async () => {
    renderPanel();

    expect(await screen.findByText(actionBrief.context)).toBeInTheDocument();
    for (const heading of [
      'Bağlam',
      'Detaylar',
      'İlgili kararlar',
      'Açık sorular',
      'Kareler',
      'Alıntılar',
      'Önerilen adımlar',
    ]) {
      expect(screen.getByRole('heading', { name: new RegExp(`^${heading}`) })).toBeInTheDocument();
    }
    expect(screen.getByText('Dosya PDF olarak gönderilecek')).toBeInTheDocument();
    expect(screen.getByText('Bütçe onaylandı')).toBeInTheDocument();
    expect(screen.getByText('Kim sunacak?')).toBeInTheDocument();
    expect(screen.getByText('Rakamları ben paylaşırım')).toBeInTheDocument();
    expect(screen.getByText('Diğerleri')).toBeInTheDocument();
    // The frame the session does not have is left out; the one it has is shown with its reason.
    expect(screen.getByText('Bütçe tablosu bu karede')).toBeInTheDocument();
    expect(screen.queryByText('Artık olmayan bir kare')).not.toBeInTheDocument();
  });

  it('marks the suggested steps with a "Öneri" badge', async () => {
    renderPanel();

    const heading = await screen.findByRole('heading', { name: /Önerilen adımlar/ });
    expect(within(heading).getByText('Öneri')).toBeInTheDocument();
    expect(screen.getByText('Taslağı önce Mehmet ile gözden geçir')).toBeInTheDocument();
    expect(screen.getByText(/toplantıda kararlaştırılmadı/)).toBeInTheDocument();
  });

  it('opens a frame in the viewer', async () => {
    const user = userEvent.setup();
    renderPanel();

    await user.click(await screen.findByRole('button', { name: /01:05 anındaki kareyi büyüt/ }));

    expect(await screen.findByRole('dialog', { name: 'Kare — 01:05' })).toBeInTheDocument();
  });

  it('moves the player to the time of a quote', async () => {
    const user = userEvent.setup();
    const { onSeek } = renderPanel();

    await user.click(await screen.findByRole('button', { name: '01:10 anına git' }));

    expect(onSeek).toHaveBeenCalledWith(70_000);
  });

  it('shows quote times as plain text when there is no audio to move', async () => {
    renderPanel({ onSeek: undefined });

    expect(await screen.findByText('01:10')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: '01:10 anına git' })).not.toBeInTheDocument();
  });

  it('copies the brief as Markdown', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.exportActionBriefMarkdown).mockResolvedValue(
      ok('# Bütçeyi paylaş\n\nBağlam'),
    );
    renderPanel();

    await user.click(await screen.findByRole('button', { name: 'Markdown olarak kopyala' }));

    expect(commands.exportActionBriefMarkdown).toHaveBeenCalledWith('a1');
    await waitFor(async () =>
      expect(await navigator.clipboard.readText()).toBe('# Bütçeyi paylaş\n\nBağlam'),
    );
    expect(await screen.findByText('Özet Markdown olarak panoya kopyalandı.')).toBeInTheDocument();
  });

  it('regenerates the brief and shows the new one', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.generateActionBrief).mockResolvedValue(
      ok({ ...actionBrief, context: 'Yeni bağlam' }),
    );
    renderPanel();

    await user.click(await screen.findByRole('button', { name: 'Yeniden oluştur' }));

    expect(commands.generateActionBrief).toHaveBeenCalledWith('a1');
    expect(await screen.findByText('Yeni bağlam')).toBeInTheDocument();
    expect(screen.queryByText(actionBrief.context)).not.toBeInTheDocument();
  });

  it('points to the providers page when no provider is assigned', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.getActionBrief).mockResolvedValue(ok(null));
    vi.mocked(commands.generateActionBrief).mockResolvedValue(fail('no_provider') as never);
    renderPanel();

    await user.click(await screen.findByRole('button', { name: 'Özet oluştur' }));

    const link = await screen.findByRole('link', { name: 'AI Sağlayıcıları sayfasına git' });
    expect(link).toHaveAttribute('href', '/providers');
    // The toast carries the failure as a localised message, not a raw code.
    const toasts = await screen.findAllByRole('alert');
    expect(
      toasts.some((toast) =>
        toast.textContent?.includes(
          'Özet için atanmış bir AI sağlayıcı yok. AI Sağlayıcıları sayfasında “Özet” görevine bir profil atayıp tekrar deneyin.',
        ),
      ),
    ).toBe(true);
    expect(screen.getByRole('button', { name: 'Özet oluştur' })).toBeInTheDocument();
  });

  it('closes with Escape', async () => {
    const user = userEvent.setup();
    const { onClose } = renderPanel();
    await screen.findByText(actionBrief.context);

    await user.keyboard('{Escape}');

    expect(onClose).toHaveBeenCalled();
  });
});

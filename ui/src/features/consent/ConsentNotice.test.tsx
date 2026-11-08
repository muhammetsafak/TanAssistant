import { screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '../../i18n';
import { commands } from '../../ipc/bindings';
import { ok, renderWithProviders } from '../../test-utils';
import { ConsentNotice } from './ConsentNotice';

vi.mock('../../ipc/bindings', () => ({
  commands: { getSettings: vi.fn(), updateSettings: vi.fn() },
}));

function settings(consentNoticeAcknowledged: boolean) {
  return {
    uiLanguage: 'tr' as const,
    defaultNoteLanguage: 'tr',
    defaultSpeechLanguage: 'auto',
    frameIntervalMs: 3000,
    frameChangeThreshold: 8,
    autoProcessAfterRecording: true,
    audioRetentionDays: null,
    consentNoticeAcknowledged,
  };
}

describe('ConsentNotice', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
    vi.mocked(commands.getSettings).mockReset();
    vi.mocked(commands.updateSettings).mockReset();
  });

  it('is shown while the notice has not been acknowledged', async () => {
    vi.mocked(commands.getSettings).mockResolvedValue(ok(settings(false)));

    renderWithProviders(<ConsentNotice />);

    const dialog = await screen.findByRole('dialog', { name: 'Kayıt hakkında bilgilendirme' });
    expect(dialog).toHaveAttribute('aria-modal', 'true');
    expect(screen.getByRole('button', { name: 'Anladım' })).toBeInTheDocument();
  });

  it('shows the existing notice text and keeps focus on the acknowledge button', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.getSettings).mockResolvedValue(ok(settings(false)));

    renderWithProviders(<ConsentNotice />);

    const dialog = await screen.findByRole('dialog');
    expect(screen.getByText('Merhaba, ben Tan.')).toBeInTheDocument();
    expect(dialog).toHaveAccessibleDescription(expect.stringContaining('KVKK'));
    expect(within(dialog).queryByRole('listitem')).not.toBeInTheDocument();
    const button = screen.getByRole('button', { name: 'Anladım' });
    expect(button).toHaveFocus();

    // The only focusable control: Tab stays on it instead of leaving the notice.
    await user.tab();
    expect(button).toHaveFocus();
  });

  it('cannot be dismissed with Escape', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.getSettings).mockResolvedValue(ok(settings(false)));
    renderWithProviders(<ConsentNotice />);
    await screen.findByRole('dialog');

    await user.keyboard('{Escape}');

    expect(screen.getByRole('dialog')).toBeInTheDocument();
  });

  it('persists the acknowledgement, closes, and is not shown again on a new render', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.getSettings).mockResolvedValue(ok(settings(false)));
    vi.mocked(commands.updateSettings).mockResolvedValue(ok(settings(true)));

    const first = renderWithProviders(<ConsentNotice />);
    await user.click(await screen.findByRole('button', { name: 'Anladım' }));

    expect(commands.updateSettings).toHaveBeenCalledWith(
      expect.objectContaining({ consentNoticeAcknowledged: true }),
    );
    await vi.waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    first.unmount();

    // The backend now reports the notice as acknowledged.
    vi.mocked(commands.getSettings).mockResolvedValue(ok(settings(true)));
    renderWithProviders(<ConsentNotice />);
    await vi.waitFor(() => expect(commands.getSettings).toHaveBeenCalledTimes(2));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('is shown with an error when the settings cannot be read', async () => {
    vi.mocked(commands.getSettings).mockResolvedValue({
      status: 'error',
      error: { code: 'internal', message: 'db locked' },
    });

    renderWithProviders(<ConsentNotice />);

    const dialog = await screen.findByRole('dialog', { name: 'Kayıt hakkında bilgilendirme' });
    expect(dialog).toHaveTextContent('Ayarlar yüklenemedi.');
    expect(screen.getByRole('button', { name: 'Yeniden dene' })).toBeInTheDocument();
  });

  it('is not shown when the notice was already acknowledged', async () => {
    vi.mocked(commands.getSettings).mockResolvedValue(ok(settings(true)));

    renderWithProviders(<ConsentNotice />);

    await vi.waitFor(() => expect(commands.getSettings).toHaveBeenCalled());
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });
});

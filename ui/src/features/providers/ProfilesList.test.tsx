import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '../../i18n';
import { commands } from '../../ipc/bindings';
import { fail, ok, renderWithProviders } from '../../test-utils';
import ProvidersPage from '../../pages/ProvidersPage';
import {
  claudeCliProfile,
  codexCliProfile,
  deepSeekProfile,
  kindInfos,
  ollamaProfile,
  openAiProfile,
  secrets,
} from './fixtures';

vi.mock('../../ipc/bindings', () => ({
  commands: {
    listProviderProfiles: vi.fn(),
    listProviderKinds: vi.fn(),
    upsertProviderProfile: vi.fn(),
    deleteProviderProfile: vi.fn(),
    testProvider: vi.fn(),
    getTaskAssignments: vi.fn(),
    setTaskAssignment: vi.fn(),
    clearTaskAssignment: vi.fn(),
    secretStoreStatus: vi.fn(),
    listSecrets: vi.fn(),
    createSecret: vi.fn(),
  },
}));

describe('ProvidersPage', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
    vi.mocked(commands.listProviderProfiles).mockResolvedValue(
      ok([openAiProfile, deepSeekProfile, ollamaProfile]),
    );
    vi.mocked(commands.listProviderKinds).mockResolvedValue(kindInfos);
    vi.mocked(commands.getTaskAssignments).mockResolvedValue(ok([]));
    vi.mocked(commands.deleteProviderProfile).mockResolvedValue(ok(null));
    vi.mocked(commands.secretStoreStatus).mockResolvedValue({
      backend: 'keyring',
      available: true,
      locked: false,
      masterPasswordSet: false,
    });
    vi.mocked(commands.listSecrets).mockResolvedValue(ok(secrets));
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it('labels the CLI profiles from the translations', async () => {
    vi.mocked(commands.listProviderProfiles).mockResolvedValue(
      ok([claudeCliProfile, codexCliProfile]),
    );
    renderWithProviders(<ProvidersPage />);

    const table = await screen.findByRole('table');
    const claudeRow = within(table).getByRole('row', { name: /Claude subscription/ });
    expect(within(claudeRow).getByText('Claude CLI (abonelik)')).toBeInTheDocument();
    const codexRow = within(table).getByRole('row', { name: /Codex subscription/ });
    expect(within(codexRow).getByText('Codex CLI (abonelik)')).toBeInTheDocument();
  });

  it('lists profiles with provider, model and capability badges', async () => {
    renderWithProviders(<ProvidersPage />);

    const table = await screen.findByRole('table');
    const deepSeekRow = within(table).getByRole('row', { name: /DeepSeek cheap/ });
    expect(within(deepSeekRow).getByText('DeepSeek')).toBeInTheDocument();
    expect(within(deepSeekRow).getByText('deepseek-chat')).toBeInTheDocument();
    expect(within(deepSeekRow).getByText('Özet')).toBeInTheDocument();
    expect(within(deepSeekRow).queryByText('Görsel')).not.toBeInTheDocument();
    expect(within(deepSeekRow).queryByText('Transkripsiyon')).not.toBeInTheDocument();

    const openAiRow = within(table).getByRole('row', { name: /OpenAI work/ });
    expect(within(openAiRow).getByText('Görsel')).toBeInTheDocument();
    expect(within(openAiRow).getByText('Transkripsiyon')).toBeInTheDocument();
  });

  it('shows an empty state with a way to add the first profile', async () => {
    vi.mocked(commands.listProviderProfiles).mockResolvedValue(ok([]));
    renderWithProviders(<ProvidersPage />);

    expect(await screen.findByText(/Henüz profil yok/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Profil ekle' })).toBeInTheDocument();
    // Assignments still work with no profile: transcription falls back to local whisper.
    expect(await screen.findByLabelText('Transkripsiyon')).toHaveDisplayValue(
      'Yerel whisper (varsayılan)',
    );
  });

  it('offers a retry when the profiles cannot be loaded', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.listProviderProfiles).mockResolvedValueOnce(fail('internal') as never);
    renderWithProviders(<ProvidersPage />);

    expect(await screen.findAllByText('Profiller yüklenemedi.')).not.toHaveLength(0);
    await user.click(screen.getAllByRole('button', { name: 'Yeniden dene' })[0]!);

    expect(await screen.findByRole('table')).toBeInTheDocument();
  });

  it('opens the form for a new profile and for editing one', async () => {
    const user = userEvent.setup();
    renderWithProviders(<ProvidersPage />);
    await screen.findByRole('table');

    await user.click(screen.getByRole('button', { name: 'Profil ekle' }));
    expect(await screen.findByRole('dialog', { name: 'Profil ekle' })).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Vazgeç' }));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Düzenle: DeepSeek cheap' }));
    const dialog = await screen.findByRole('dialog', { name: 'Profili düzenle' });
    expect(await within(dialog).findByLabelText('Model')).toHaveValue('deepseek-chat');
  });

  it('does not delete without confirmation, and deletes after it', async () => {
    const user = userEvent.setup();
    renderWithProviders(<ProvidersPage />);
    await screen.findByRole('table');

    await user.click(screen.getByRole('button', { name: 'Sil: DeepSeek cheap' }));
    const dialog = screen.getByRole('dialog', { name: 'Profil silinsin mi?' });
    await user.click(within(dialog).getByRole('button', { name: 'Vazgeç' }));
    expect(commands.deleteProviderProfile).not.toHaveBeenCalled();

    await user.click(screen.getByRole('button', { name: 'Sil: DeepSeek cheap' }));
    await user.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'Sil' }));

    await waitFor(() => expect(commands.deleteProviderProfile).toHaveBeenCalledWith('p-deepseek'));
    // The assignments refresh too, because deleting a profile removes its assignments.
    await waitFor(() => expect(vi.mocked(commands.getTaskAssignments).mock.calls.length).toBe(2));
  });

  it('runs the connection test for the row it was clicked in', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.testProvider).mockResolvedValue(
      ok({ latencyMs: 90, availableModels: [], modelFound: null, replyPreview: 'ok' }),
    );
    renderWithProviders(<ProvidersPage />);
    await screen.findByRole('table');

    await user.click(screen.getByRole('button', { name: 'Bağlantıyı test et: DeepSeek cheap' }));

    expect(commands.testProvider).toHaveBeenCalledWith('p-deepseek');
    const deepSeekRow = screen.getByRole('row', { name: /DeepSeek cheap/ });
    expect(await within(deepSeekRow).findByRole('status')).toHaveTextContent('Bağlantı başarılı.');
    expect(
      within(screen.getByRole('row', { name: /OpenAI work/ })).queryByRole('status'),
    ).not.toBeInTheDocument();
  });
});

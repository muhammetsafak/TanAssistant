import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '../../i18n';
import { commands } from '../../ipc/bindings';
import { fail, ok, renderWithProviders } from '../../test-utils';
import {
  deepSeekProfile,
  deepSeekSecret,
  kindInfos,
  ollamaProfile,
  openAiProfile,
  openAiSecret,
  secrets,
} from './fixtures';
import { ProfileForm, type ProfileTarget } from './ProfileForm';

vi.mock('../../ipc/bindings', () => ({
  commands: {
    listProviderKinds: vi.fn(),
    upsertProviderProfile: vi.fn(),
    listProviderProfiles: vi.fn(),
    secretStoreStatus: vi.fn(),
    listSecrets: vi.fn(),
    createSecret: vi.fn(),
  },
}));

const unlockedVault = {
  backend: 'keyring' as const,
  available: true,
  locked: false,
  masterPasswordSet: false,
};

async function openForm(target: ProfileTarget = 'new') {
  const onClose = vi.fn();
  const result = renderWithProviders(<ProfileForm target={target} onClose={onClose} />);
  await screen.findByLabelText('Görünen ad');
  // The dialog is rebuilt once the provider information has loaded, so look it up afterwards.
  const dialog = screen.getByRole('dialog');
  return { ...result, dialog, onClose };
}

describe('ProfileForm', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
    vi.mocked(commands.listProviderKinds).mockResolvedValue(kindInfos);
    vi.mocked(commands.secretStoreStatus).mockResolvedValue(unlockedVault);
    vi.mocked(commands.listSecrets).mockResolvedValue(ok(secrets));
    vi.mocked(commands.upsertProviderProfile).mockResolvedValue(ok(openAiProfile));
    vi.mocked(commands.createSecret).mockResolvedValue(ok(openAiSecret));
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it('does not save while the required fields are empty and names each problem', async () => {
    const user = userEvent.setup();
    const { dialog } = await openForm();

    await user.click(within(dialog).getByRole('button', { name: 'Kaydet' }));

    expect(commands.upsertProviderProfile).not.toHaveBeenCalled();
    expect(within(dialog).getByText('Görünen ad zorunludur.')).toBeInTheDocument();
    expect(within(dialog).getByText('Model zorunludur.')).toBeInTheDocument();
    expect(within(dialog).getByText('Bu sağlayıcı için bir secret seçin.')).toBeInTheDocument();
    expect(within(dialog).getByLabelText('Görünen ad')).toHaveAttribute('aria-invalid', 'true');
    // Focus moves to the first invalid field.
    expect(within(dialog).getByLabelText('Görünen ad')).toHaveFocus();
  });

  it('rejects a base URL that is not http(s)', async () => {
    const user = userEvent.setup();
    const { dialog } = await openForm();

    await user.type(within(dialog).getByLabelText('Görünen ad'), 'X');
    await user.type(within(dialog).getByLabelText('Model'), 'gpt-4o');
    await user.selectOptions(await within(dialog).findByLabelText(/API anahtarı/), 's1');
    await user.type(within(dialog).getByLabelText('Base URL'), 'ftp://nope');
    await user.click(within(dialog).getByRole('button', { name: 'Kaydet' }));

    expect(commands.upsertProviderProfile).not.toHaveBeenCalled();
    expect(
      within(dialog).getByText('Geçerli bir http:// ya da https:// adresi girin.'),
    ).toBeInTheDocument();
  });

  it('saves a new profile with the picked model suggestion and secret', async () => {
    const user = userEvent.setup();
    const { dialog, onClose } = await openForm();

    await user.type(within(dialog).getByLabelText('Görünen ad'), '  OpenAI work ');
    await user.click(within(dialog).getByRole('button', { name: 'gpt-4o' }));
    expect(within(dialog).getByLabelText('Model')).toHaveValue('gpt-4o');
    await waitFor(() =>
      expect(within(dialog).getByRole('option', { name: 'OPENAI_KEY' })).toBeInTheDocument(),
    );
    await user.selectOptions(within(dialog).getByLabelText(/API anahtarı/), 's1');
    await user.click(within(dialog).getByRole('button', { name: 'Kaydet' }));

    await waitFor(() =>
      expect(commands.upsertProviderProfile).toHaveBeenCalledWith({
        id: null,
        kind: 'openai',
        displayName: 'OpenAI work',
        model: 'gpt-4o',
        baseUrl: null,
        secretRef: 's1',
        supportsVision: false,
      }),
    );
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(await screen.findByText('Profil kaydedildi.')).toBeInTheDocument();
  });

  it('pre-fills the Ollama endpoint, needs no secret and sends the vision flag', async () => {
    const user = userEvent.setup();
    const { dialog } = await openForm();

    await user.selectOptions(within(dialog).getByLabelText('Sağlayıcı'), 'ollama');

    expect(within(dialog).getByLabelText('Base URL')).toHaveValue('http://localhost:11434');
    const vision = within(dialog).getByRole('checkbox', { name: /vision destekli/ });
    expect(vision).not.toBeChecked();
    expect(within(dialog).getByLabelText(/isteğe bağlı/)).toBeInTheDocument();

    await user.type(within(dialog).getByLabelText('Görünen ad'), 'Local llava');
    await user.click(within(dialog).getByRole('button', { name: 'llava' }));
    await user.click(vision);
    await user.click(within(dialog).getByRole('button', { name: 'Kaydet' }));

    await waitFor(() =>
      expect(commands.upsertProviderProfile).toHaveBeenCalledWith({
        id: null,
        kind: 'ollama',
        displayName: 'Local llava',
        model: 'llava',
        baseUrl: null,
        secretRef: null,
        supportsVision: true,
      }),
    );
  });

  it('drops the Ollama endpoint and vision flag when switching to another provider', async () => {
    const user = userEvent.setup();
    const { dialog } = await openForm();

    await user.selectOptions(within(dialog).getByLabelText('Sağlayıcı'), 'ollama');
    await user.click(within(dialog).getByRole('checkbox', { name: /vision destekli/ }));
    await user.selectOptions(within(dialog).getByLabelText('Sağlayıcı'), 'deepseek');

    expect(within(dialog).getByLabelText('Base URL')).toHaveValue('');
    expect(within(dialog).queryByRole('checkbox')).not.toBeInTheDocument();
  });

  it('edits an existing profile in place and never loads a secret value', async () => {
    const user = userEvent.setup();
    const { dialog } = await openForm(deepSeekProfile);

    expect(within(dialog).getByLabelText('Sağlayıcı')).toBeDisabled();
    expect(within(dialog).getByLabelText('Görünen ad')).toHaveValue('DeepSeek cheap');
    await waitFor(() => expect(within(dialog).getByLabelText(/API anahtarı/)).toHaveValue('s2'));
    expect(commands.listSecrets).toHaveBeenCalled();

    await user.clear(within(dialog).getByLabelText('Görünen ad'));
    await user.type(within(dialog).getByLabelText('Görünen ad'), 'DeepSeek v2');
    await user.click(within(dialog).getByRole('button', { name: 'Kaydet' }));

    await waitFor(() =>
      expect(commands.upsertProviderProfile).toHaveBeenCalledWith({
        id: 'p-deepseek',
        kind: 'deepseek',
        displayName: 'DeepSeek v2',
        model: 'deepseek-chat',
        baseUrl: null,
        secretRef: 's2',
        supportsVision: false,
      }),
    );
  });

  it('shows an existing Ollama profile with its default endpoint filled in', async () => {
    const { dialog } = await openForm(ollamaProfile);

    expect(within(dialog).getByLabelText('Base URL')).toHaveValue('http://localhost:11434');
  });

  it('warns when the profile points at a secret that no longer exists', async () => {
    vi.mocked(commands.listSecrets).mockResolvedValue(ok([deepSeekSecret]));
    const { dialog } = await openForm(openAiProfile);

    expect(await within(dialog).findByRole('alert')).toHaveTextContent('Seçili secret artık yok.');
  });

  it('creates a secret from the form and selects it', async () => {
    const user = userEvent.setup();
    const created = { id: 's3', name: 'NEW_KEY', description: '', createdAt: 3 };
    vi.mocked(commands.createSecret).mockImplementation((async () => {
      vi.mocked(commands.listSecrets).mockResolvedValue(ok([...secrets, created]));
      return ok(created);
    }) as never);
    const { dialog } = await openForm();
    await waitFor(() =>
      expect(within(dialog).getByRole('option', { name: 'OPENAI_KEY' })).toBeInTheDocument(),
    );

    await user.click(within(dialog).getByRole('button', { name: 'Yeni secret oluştur' }));
    const secretDialog = await screen.findByRole('dialog', { name: 'Secret ekle' });
    await user.type(within(secretDialog).getByLabelText('Ad'), 'NEW_KEY');
    await user.type(within(secretDialog).getByLabelText('Değer'), 'sk-123');
    await user.click(within(secretDialog).getByRole('button', { name: 'Kaydet' }));

    await waitFor(() => expect(screen.queryByRole('dialog', { name: 'Secret ekle' })).toBeNull());
    await waitFor(() => expect(within(dialog).getByLabelText(/API anahtarı/)).toHaveValue('s3'));
  });

  it('selects nothing after creating a secret when the list was not loaded beforehand', async () => {
    const user = userEvent.setup();
    const created = { id: 's3', name: 'NEW_KEY', description: '', createdAt: 3 };
    vi.mocked(commands.listSecrets).mockResolvedValueOnce(fail('internal') as never);
    vi.mocked(commands.createSecret).mockImplementation((async () => {
      vi.mocked(commands.listSecrets).mockResolvedValue(ok([...secrets, created]));
      return ok(created);
    }) as never);
    const { dialog } = await openForm();
    expect(await within(dialog).findByText('Secret listesi yüklenemedi.')).toBeInTheDocument();

    await user.click(within(dialog).getByRole('button', { name: 'Yeni secret oluştur' }));
    const secretDialog = await screen.findByRole('dialog', { name: 'Secret ekle' });
    await user.type(within(secretDialog).getByLabelText('Ad'), 'NEW_KEY');
    await user.type(within(secretDialog).getByLabelText('Değer'), 'sk-123');
    await user.click(within(secretDialog).getByRole('button', { name: 'Kaydet' }));

    await waitFor(() => expect(screen.queryByRole('dialog', { name: 'Secret ekle' })).toBeNull());
    expect(await within(dialog).findByRole('option', { name: 'NEW_KEY' })).toBeInTheDocument();
    // Guessing "the first secret" here would pick OPENAI_KEY for the wrong provider.
    expect(within(dialog).getByLabelText(/API anahtarı/)).toHaveValue('');
  });

  it('explains a failed vault status read and offers a retry', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.secretStoreStatus).mockRejectedValueOnce(new Error('boom'));
    const { dialog } = await openForm();

    const alert = await within(dialog).findByRole('alert');
    expect(alert).toHaveTextContent('Kasa durumu okunamadı');
    expect(within(dialog).getByRole('button', { name: 'Yeni secret oluştur' })).toBeDisabled();

    await user.click(within(alert).getByRole('button', { name: 'Yeniden dene' }));

    await waitFor(() => expect(within(dialog).queryByRole('alert')).not.toBeInTheDocument());
    expect(within(dialog).getByRole('button', { name: 'Yeni secret oluştur' })).toBeEnabled();
    expect(await within(dialog).findByRole('option', { name: 'OPENAI_KEY' })).toBeInTheDocument();
  });

  it('points to the Credentials page instead of listing secrets while the vault is locked', async () => {
    vi.mocked(commands.secretStoreStatus).mockResolvedValue({
      ...unlockedVault,
      backend: 'encrypted_file',
      locked: true,
      masterPasswordSet: true,
    });
    const { dialog } = await openForm();

    expect(await within(dialog).findByText(/Kasa kilitli/)).toBeInTheDocument();
    expect(within(dialog).getByRole('link', { name: 'Credentials sayfasına git' })).toHaveAttribute(
      'href',
      '/credentials',
    );
    expect(within(dialog).getByRole('button', { name: 'Yeni secret oluştur' })).toBeDisabled();
  });

  it('reports a rejected save by its error code and keeps the form open', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.upsertProviderProfile).mockResolvedValue(fail('validation') as never);
    const { dialog, onClose } = await openForm(openAiProfile);
    await waitFor(() => expect(within(dialog).getByLabelText(/API anahtarı/)).toHaveValue('s1'));

    await user.click(within(dialog).getByRole('button', { name: 'Kaydet' }));

    expect(await screen.findByText(/Girilen bilgiler geçersiz/)).toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole('dialog')).toBeInTheDocument();
  });

  it('offers a retry when the provider information cannot be loaded', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.listProviderKinds).mockRejectedValueOnce(new Error('boom'));
    renderWithProviders(<ProfileForm target="new" onClose={vi.fn()} />);

    expect(await screen.findByRole('alert')).toHaveTextContent('Sağlayıcı bilgileri yüklenemedi.');
    await user.click(screen.getByRole('button', { name: 'Yeniden dene' }));

    expect(await screen.findByLabelText('Görünen ad')).toBeInTheDocument();
  });
});

import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '../../i18n';
import { commands } from '../../ipc/bindings';
import { fail, ok, renderWithProviders } from '../../test-utils';
import CredentialsPage from './CredentialsPage';

vi.mock('../../ipc/bindings', () => ({
  commands: {
    listVariables: vi.fn(),
    listSecrets: vi.fn(),
    secretStoreStatus: vi.fn(),
    unlockSecretStore: vi.fn(),
    setMasterPassword: vi.fn(),
  },
}));

const lockedFileVault = {
  backend: 'encrypted_file' as const,
  available: true,
  locked: true,
  masterPasswordSet: true,
};

describe('CredentialsPage vault states', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
    vi.mocked(commands.listVariables).mockResolvedValue(ok([]));
    vi.mocked(commands.listSecrets).mockResolvedValue(ok([]));
  });

  afterEach(() => vi.clearAllMocks());

  it('shows the secret list when the OS vault is available', async () => {
    vi.mocked(commands.secretStoreStatus).mockResolvedValue({
      backend: 'keyring',
      available: true,
      locked: false,
      masterPasswordSet: false,
    });
    renderWithProviders(<CredentialsPage />);

    expect(await screen.findByText('Henüz secret yok.')).toBeInTheDocument();
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('asks for the master password when the file vault is locked, and unlocks it', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.secretStoreStatus).mockResolvedValueOnce(lockedFileVault);
    vi.mocked(commands.unlockSecretStore).mockResolvedValue(
      ok({ ...lockedFileVault, locked: false }),
    );
    renderWithProviders(<CredentialsPage />);

    const dialog = await screen.findByRole('dialog', { name: 'Kasanın kilidini aç' });
    expect(screen.queryByText('Henüz secret yok.')).not.toBeInTheDocument();
    vi.mocked(commands.secretStoreStatus).mockResolvedValue({ ...lockedFileVault, locked: false });

    const password = within(dialog).getByLabelText('Ana parola');
    expect(password).toHaveAttribute('type', 'password');
    await user.type(password, 'correct horse');
    await user.click(within(dialog).getByRole('button', { name: 'Kilidi aç' }));

    await waitFor(() => expect(commands.unlockSecretStore).toHaveBeenCalledWith('correct horse'));
    expect(await screen.findByText('Henüz secret yok.')).toBeInTheDocument();
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('keeps the dialog open with an inline message on a wrong password', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.secretStoreStatus).mockResolvedValue(lockedFileVault);
    vi.mocked(commands.unlockSecretStore).mockResolvedValue(fail('invalid_password') as never);
    renderWithProviders(<CredentialsPage />);

    const dialog = await screen.findByRole('dialog');
    await user.type(within(dialog).getByLabelText('Ana parola'), 'wrong');
    await user.click(within(dialog).getByRole('button', { name: 'Kilidi aç' }));

    expect(await within(dialog).findByRole('alert')).toHaveTextContent('Ana parola yanlış.');
    expect(screen.getByRole('dialog')).toBeInTheDocument();
  });

  it('lets the user reopen the dialog from the vault panel after cancelling', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.secretStoreStatus).mockResolvedValue(lockedFileVault);
    renderWithProviders(<CredentialsPage />);

    const dialog = await screen.findByRole('dialog');
    await user.click(within(dialog).getByRole('button', { name: 'Vazgeç' }));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Kilidi aç' }));
    expect(screen.getByRole('dialog')).toBeInTheDocument();
  });

  it('asks to set a master password first when none exists, and checks the confirmation', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.secretStoreStatus).mockResolvedValue({
      backend: 'encrypted_file',
      available: true,
      locked: true,
      masterPasswordSet: false,
    });
    vi.mocked(commands.setMasterPassword).mockResolvedValue(ok(lockedFileVault));
    renderWithProviders(<CredentialsPage />);

    const dialog = await screen.findByRole('dialog', { name: 'Ana parola belirle' });
    await user.type(within(dialog).getByLabelText('Yeni ana parola'), 'abc');
    await user.type(within(dialog).getByLabelText('Ana parolayı doğrulayın'), 'abd');
    await user.click(within(dialog).getByRole('button', { name: 'Parolayı belirle' }));
    expect(within(dialog).getByRole('alert')).toHaveTextContent('Parolalar eşleşmiyor.');
    expect(commands.setMasterPassword).not.toHaveBeenCalled();

    await user.clear(within(dialog).getByLabelText('Ana parolayı doğrulayın'));
    await user.type(within(dialog).getByLabelText('Ana parolayı doğrulayın'), 'abc');
    await user.click(within(dialog).getByRole('button', { name: 'Parolayı belirle' }));
    await waitFor(() => expect(commands.setMasterPassword).toHaveBeenCalledWith('abc', null));
  });

  it('explains that the vault is unavailable and keeps variables usable', async () => {
    vi.mocked(commands.secretStoreStatus).mockResolvedValue({
      backend: 'keyring',
      available: false,
      locked: false,
      masterPasswordSet: false,
    });
    renderWithProviders(<CredentialsPage />);

    expect(await screen.findByText('Gizli bilgi kasası kullanılamıyor')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Variable ekle' })).toBeEnabled();
    expect(screen.queryByRole('button', { name: 'Secret ekle' })).not.toBeInTheDocument();
  });

  it('does not keep the master password in the mutation cache after unlocking', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.secretStoreStatus).mockResolvedValueOnce(lockedFileVault);
    vi.mocked(commands.unlockSecretStore).mockResolvedValue(
      ok({ ...lockedFileVault, locked: false }),
    );
    const { queryClient } = renderWithProviders(<CredentialsPage />);

    const dialog = await screen.findByRole('dialog');
    vi.mocked(commands.secretStoreStatus).mockResolvedValue({ ...lockedFileVault, locked: false });
    await user.type(within(dialog).getByLabelText('Ana parola'), 'master-needle-55');
    await user.click(within(dialog).getByRole('button', { name: 'Kilidi aç' }));

    await waitFor(() => expect(commands.unlockSecretStore).toHaveBeenCalled());
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    await waitFor(() => {
      const held = queryClient
        .getMutationCache()
        .getAll()
        .map((mutation) => JSON.stringify(mutation.state.variables ?? null))
        .join('');
      expect(held).not.toContain('master-needle-55');
    });
  });
});

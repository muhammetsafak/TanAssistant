import { act, fireEvent, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '../../i18n';
import { commands } from '../../ipc/bindings';
import { fail, ok, renderWithProviders } from '../../test-utils';
import { REVEAL_DURATION_MS } from './api';
import { SecretsTable } from './SecretsTable';

vi.mock('../../ipc/bindings', () => ({
  commands: {
    listSecrets: vi.fn(),
    revealSecret: vi.fn(),
    createSecret: vi.fn(),
    updateSecret: vi.fn(),
    deleteSecret: vi.fn(),
    secretStoreStatus: vi.fn(),
  },
}));

const SECRET_VALUE = 'test-secret-value-9f3a';
const secret = {
  id: 's1',
  name: 'OPENAI_KEY',
  description: 'OpenAI API key',
  createdAt: Date.UTC(2026, 10, 5, 9, 30),
};

async function renderTable() {
  const result = renderWithProviders(<SecretsTable />);
  await screen.findByText('OPENAI_KEY');
  return result;
}

describe('SecretsTable', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
    vi.mocked(commands.listSecrets).mockResolvedValue(ok([secret]));
    vi.mocked(commands.revealSecret).mockResolvedValue(ok(SECRET_VALUE));
    vi.mocked(commands.deleteSecret).mockResolvedValue(ok(null));
    vi.mocked(commands.createSecret).mockResolvedValue(ok(secret));
    vi.mocked(commands.updateSecret).mockResolvedValue(ok(secret));
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.clearAllMocks();
  });

  it('does not put the secret value in the DOM on first render', async () => {
    await renderTable();

    expect(document.body).not.toHaveTextContent(SECRET_VALUE);
    expect(screen.getByText('••••••')).toBeInTheDocument();
    expect(commands.revealSecret).not.toHaveBeenCalled();
  });

  it('shows the value after "Göster" and masks it again when the time is up', async () => {
    await renderTable();
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });

    fireEvent.click(screen.getByRole('button', { name: 'Göster: OPENAI_KEY' }));
    await act(async () => {});

    expect(commands.revealSecret).toHaveBeenCalledWith('s1');
    expect(screen.getByText(SECRET_VALUE)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Gizle: OPENAI_KEY' })).toBeInTheDocument();

    await act(async () => {
      vi.advanceTimersByTime(REVEAL_DURATION_MS - 1);
    });
    expect(screen.getByText(SECRET_VALUE)).toBeInTheDocument();

    await act(async () => {
      vi.advanceTimersByTime(1);
    });
    expect(screen.queryByText(SECRET_VALUE)).not.toBeInTheDocument();
    expect(screen.getByText('••••••')).toBeInTheDocument();
  });

  it('hides the value right away with "Gizle"', async () => {
    const user = userEvent.setup();
    await renderTable();

    await user.click(screen.getByRole('button', { name: 'Göster: OPENAI_KEY' }));
    expect(await screen.findByText(SECRET_VALUE)).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Gizle: OPENAI_KEY' }));
    expect(screen.queryByText(SECRET_VALUE)).not.toBeInTheDocument();
  });

  it('copies the value to the clipboard without showing it', async () => {
    const user = userEvent.setup();
    const writeText = vi.fn().mockResolvedValue(undefined);
    await renderTable();
    // user-event installs its own clipboard stub on setup; replace it with a spy.
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });

    await user.click(screen.getByRole('button', { name: 'Kopyala: OPENAI_KEY' }));

    await waitFor(() => expect(writeText).toHaveBeenCalledWith(SECRET_VALUE));
    expect(screen.queryByText(SECRET_VALUE)).not.toBeInTheDocument();
    expect(await screen.findByText('Değer panoya kopyalandı.')).toBeInTheDocument();
  });

  it('does not delete without confirmation', async () => {
    const user = userEvent.setup();
    await renderTable();

    await user.click(screen.getByRole('button', { name: 'Sil: OPENAI_KEY' }));

    const dialog = screen.getByRole('dialog', { name: 'Secret silinsin mi?' });
    expect(commands.deleteSecret).not.toHaveBeenCalled();

    await user.click(within(dialog).getByRole('button', { name: 'Vazgeç' }));
    expect(commands.deleteSecret).not.toHaveBeenCalled();
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('deletes after the confirmation', async () => {
    const user = userEvent.setup();
    await renderTable();

    await user.click(screen.getByRole('button', { name: 'Sil: OPENAI_KEY' }));
    const dialog = screen.getByRole('dialog');
    await user.click(within(dialog).getByRole('button', { name: 'Sil' }));

    await waitFor(() => expect(commands.deleteSecret).toHaveBeenCalledWith('s1'));
  });

  it('creates a secret from the dialog and sends the value only to the backend', async () => {
    const user = userEvent.setup();
    await renderTable();

    await user.click(screen.getByRole('button', { name: 'Secret ekle' }));
    const dialog = screen.getByRole('dialog', { name: 'Secret ekle' });
    await user.type(within(dialog).getByLabelText('Ad'), 'GEMINI_KEY');
    await user.type(within(dialog).getByLabelText('Açıklama'), 'Gemini');
    const valueInput = within(dialog).getByLabelText('Değer');
    expect(valueInput).toHaveAttribute('type', 'password');
    await user.type(valueInput, 'abc-123');
    await user.click(within(dialog).getByRole('button', { name: 'Kaydet' }));

    await waitFor(() =>
      expect(commands.createSecret).toHaveBeenCalledWith({
        name: 'GEMINI_KEY',
        description: 'Gemini',
        value: 'abc-123',
      }),
    );
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
  });

  it('requires a value when creating but not when replacing', async () => {
    const user = userEvent.setup();
    await renderTable();

    await user.click(screen.getByRole('button', { name: 'Secret ekle' }));
    let dialog = screen.getByRole('dialog');
    await user.type(within(dialog).getByLabelText('Ad'), 'X');
    await user.click(within(dialog).getByRole('button', { name: 'Kaydet' }));
    expect(within(dialog).getByText('Değer zorunludur.')).toBeInTheDocument();
    expect(commands.createSecret).not.toHaveBeenCalled();
    await user.click(within(dialog).getByRole('button', { name: 'Vazgeç' }));

    await user.click(screen.getByRole('button', { name: 'Değiştir: OPENAI_KEY' }));
    dialog = screen.getByRole('dialog', { name: 'Secret değiştir' });
    // The current value is never preloaded into the form.
    expect(within(dialog).getByLabelText('Yeni değer')).toHaveValue('');
    await user.click(within(dialog).getByRole('button', { name: 'Kaydet' }));

    await waitFor(() =>
      expect(commands.updateSecret).toHaveBeenCalledWith({
        id: 's1',
        name: 'OPENAI_KEY',
        description: 'OpenAI API key',
        value: null,
      }),
    );
  });

  it('shows a localised error when the backend rejects a write', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.deleteSecret).mockResolvedValue(fail('not_found') as never);
    await renderTable();

    await user.click(screen.getByRole('button', { name: 'Sil: OPENAI_KEY' }));
    await user.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'Sil' }));

    expect(await screen.findByRole('alert')).toHaveTextContent('Kayıt bulunamadı.');
  });

  it('keeps the secret value out of the mutation cache once the form is gone', async () => {
    const user = userEvent.setup();
    const { queryClient } = await renderTable();

    await user.click(screen.getByRole('button', { name: 'Secret ekle' }));
    const dialog = screen.getByRole('dialog');
    await user.type(within(dialog).getByLabelText('Ad'), 'GEMINI_KEY');
    await user.type(within(dialog).getByLabelText('Değer'), 'plaintext-needle-77');
    await user.click(within(dialog).getByRole('button', { name: 'Kaydet' }));

    await waitFor(() => expect(commands.createSecret).toHaveBeenCalled());
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    await waitFor(() => {
      const held = queryClient
        .getMutationCache()
        .getAll()
        .map((mutation) => JSON.stringify(mutation.state.variables ?? null))
        .join('');
      expect(held).not.toContain('plaintext-needle-77');
    });
  });

  it('masks a revealed value when the secret is replaced and never copies the stale one', async () => {
    const user = userEvent.setup();
    const writeText = vi.fn().mockResolvedValue(undefined);
    await renderTable();
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });

    await user.click(screen.getByRole('button', { name: 'Göster: OPENAI_KEY' }));
    expect(await screen.findByText(SECRET_VALUE)).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Değiştir: OPENAI_KEY' }));
    const dialog = screen.getByRole('dialog');
    await user.type(within(dialog).getByLabelText('Yeni değer'), 'replacement-value');
    await user.click(within(dialog).getByRole('button', { name: 'Kaydet' }));

    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(screen.queryByText(SECRET_VALUE)).not.toBeInTheDocument();
    expect(screen.getByText('••••••')).toBeInTheDocument();

    vi.mocked(commands.revealSecret).mockResolvedValue(ok('replacement-value'));
    await user.click(screen.getByRole('button', { name: 'Kopyala: OPENAI_KEY' }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith('replacement-value'));
  });

  it('keeps the dialog open while a write is in flight and still reports its failure', async () => {
    const user = userEvent.setup();
    let finish: (value: unknown) => void = () => {};
    vi.mocked(commands.createSecret).mockReturnValue(
      new Promise((resolve) => {
        finish = resolve;
      }) as never,
    );
    await renderTable();

    await user.click(screen.getByRole('button', { name: 'Secret ekle' }));
    const dialog = screen.getByRole('dialog');
    await user.type(within(dialog).getByLabelText('Ad'), 'DUP');
    await user.type(within(dialog).getByLabelText('Değer'), 'v');
    await user.click(within(dialog).getByRole('button', { name: 'Kaydet' }));
    await waitFor(() => expect(commands.createSecret).toHaveBeenCalled());

    await user.keyboard('{Escape}');
    expect(screen.getByRole('dialog')).toBeInTheDocument();

    finish(fail('conflict'));
    expect(await screen.findByRole('alert')).toHaveTextContent(
      'Bu ad ya da anahtar zaten kullanılıyor.',
    );
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });
});

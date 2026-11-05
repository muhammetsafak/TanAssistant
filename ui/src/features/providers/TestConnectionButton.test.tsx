import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '../../i18n';
import { commands } from '../../ipc/bindings';
import { fail, ok, renderWithProviders } from '../../test-utils';
import { ollamaProfile, openAiProfile } from './fixtures';
import { TestConnectionButton } from './TestConnectionButton';

vi.mock('../../ipc/bindings', () => ({
  commands: { testProvider: vi.fn() },
}));

describe('TestConnectionButton', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it('shows a busy state while the test runs, then the success', async () => {
    const user = userEvent.setup();
    let finish: (value: unknown) => void = () => {};
    vi.mocked(commands.testProvider).mockReturnValue(
      new Promise((resolve) => {
        finish = resolve;
      }) as never,
    );
    renderWithProviders(<TestConnectionButton profile={openAiProfile} />);

    await user.click(screen.getByRole('button', { name: 'Bağlantıyı test et: OpenAI work' }));

    expect(commands.testProvider).toHaveBeenCalledWith('p-openai');
    expect(screen.getByRole('button', { name: /OpenAI work/ })).toBeDisabled();
    expect(screen.getAllByText('Test ediliyor…').length).toBeGreaterThan(0);

    finish(ok({ latencyMs: 312, availableModels: [], modelFound: null, replyPreview: 'pong' }));

    await screen.findByText('Bağlantı başarılı.');
    const status = screen.getByRole('status');
    expect(status).toHaveTextContent('312 ms');
    expect(status).toHaveTextContent('pong');
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: /OpenAI work/ })).toBeEnabled();
  });

  it('shows a different, actionable message for an Auth failure', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.testProvider).mockResolvedValue(
      fail('provider_auth', 'HTTP 401 invalid api key') as never,
    );
    renderWithProviders(<TestConnectionButton profile={openAiProfile} />);

    await user.click(screen.getByRole('button', { name: /OpenAI work/ }));

    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent('Bağlantı kurulamadı.');
    expect(alert).toHaveTextContent('API anahtarı reddedildi');
    expect(alert).toHaveTextContent('HTTP 401 invalid api key');
    expect(screen.queryByText('Bağlantı başarılı.')).not.toBeInTheDocument();
  });

  it.each([
    ['provider_network', 'Sağlayıcıya ulaşılamadı'],
    ['provider_timeout', 'Sağlayıcı zamanında yanıt vermedi'],
    ['provider_rate_limited', 'istek sınırına ulaştı'],
    ['secret_unavailable', 'kasadan okunamadı'],
  ])('maps %s to its own message', async (code, text) => {
    const user = userEvent.setup();
    vi.mocked(commands.testProvider).mockResolvedValue(fail(code) as never);
    renderWithProviders(<TestConnectionButton profile={openAiProfile} />);

    await user.click(screen.getByRole('button', { name: /OpenAI work/ }));

    expect(await screen.findByRole('alert')).toHaveTextContent(text);
  });

  it('tells an Ollama user when the server is up but the model is not installed', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.testProvider).mockResolvedValue(
      ok({ latencyMs: 20, availableModels: ['mistral'], modelFound: false, replyPreview: null }),
    );
    renderWithProviders(<TestConnectionButton profile={ollamaProfile} />);

    await user.click(screen.getByRole('button', { name: /Local llama/ }));

    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent('Sunucuya ulaşıldı ama model yok.');
    expect(alert).toHaveTextContent('“llama3.1” modeli bu sunucuda kurulu değil.');
    expect(alert).toHaveTextContent('Kurulu modeller: mistral.');
  });

  it('confirms an installed Ollama model', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.testProvider).mockResolvedValue(
      ok({ latencyMs: 20, availableModels: ['llama3.1'], modelFound: true, replyPreview: null }),
    );
    renderWithProviders(<TestConnectionButton profile={ollamaProfile} />);

    await user.click(screen.getByRole('button', { name: /Local llama/ }));

    expect(await screen.findByRole('status')).toHaveTextContent('Model sunucuda kurulu.');
  });
});

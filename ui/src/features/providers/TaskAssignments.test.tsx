import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '../../i18n';
import { commands } from '../../ipc/bindings';
import { fail, ok, renderWithProviders } from '../../test-utils';
import { anthropicProfile, deepSeekProfile, ollamaProfile, openAiProfile } from './fixtures';
import { TaskAssignments } from './TaskAssignments';

vi.mock('../../ipc/bindings', () => ({
  commands: {
    listProviderProfiles: vi.fn(),
    getTaskAssignments: vi.fn(),
    setTaskAssignment: vi.fn(),
    clearTaskAssignment: vi.fn(),
  },
}));

function option(select: HTMLElement, name: RegExp | string) {
  return within(select).getByRole('option', { name });
}

async function renderAssignments() {
  const result = renderWithProviders(<TaskAssignments />);
  await screen.findByLabelText('Transkripsiyon');
  return result;
}

describe('TaskAssignments', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
    vi.mocked(commands.listProviderProfiles).mockResolvedValue(
      ok([openAiProfile, deepSeekProfile, ollamaProfile]),
    );
    vi.mocked(commands.getTaskAssignments).mockResolvedValue(ok([]));
    vi.mocked(commands.setTaskAssignment).mockImplementation(((task: string, profileId: string) =>
      Promise.resolve(ok({ task, profileId }))) as never);
    vi.mocked(commands.clearTaskAssignment).mockResolvedValue(ok(null));
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it('disables a DeepSeek profile for image analysis and transcription, with the reason', async () => {
    await renderAssignments();

    const vision = screen.getByLabelText('Görsel analiz');
    const transcription = screen.getByLabelText('Transkripsiyon');
    const summary = screen.getByLabelText('Özet');

    const visionOption = option(vision, /DeepSeek cheap/);
    expect(visionOption).toBeDisabled();
    expect(visionOption).toHaveTextContent('DeepSeek görsel analiz yapamaz');
    expect(visionOption).toHaveAttribute('title', 'DeepSeek görsel analiz yapamaz');

    const transcriptionOption = option(transcription, /DeepSeek cheap/);
    expect(transcriptionOption).toBeDisabled();
    expect(transcriptionOption).toHaveTextContent('DeepSeek transkripsiyon yapamaz');

    expect(option(summary, /DeepSeek cheap/)).toBeEnabled();
    expect(option(vision, /OpenAI work/)).toBeEnabled();
    expect(option(transcription, /OpenAI work/)).toBeEnabled();
  });

  it('disables an Anthropic profile for transcription only', async () => {
    vi.mocked(commands.listProviderProfiles).mockResolvedValue(ok([anthropicProfile]));
    await renderAssignments();

    const transcriptionOption = option(screen.getByLabelText('Transkripsiyon'), /Claude work/);
    expect(transcriptionOption).toBeDisabled();
    expect(transcriptionOption).toHaveTextContent('Anthropic transkripsiyon yapamaz');
    expect(option(screen.getByLabelText('Görsel analiz'), /Claude work/)).toBeEnabled();
    expect(option(screen.getByLabelText('Özet'), /Claude work/)).toBeEnabled();
  });

  it('keeps an Ollama profile off image analysis until it is marked vision-capable', async () => {
    vi.mocked(commands.listProviderProfiles).mockResolvedValue(
      ok([
        ollamaProfile,
        {
          ...ollamaProfile,
          id: 'p-llava',
          displayName: 'Local llava',
          supportsVision: true,
          capabilities: { chat: true, vision: true, transcription: false },
        },
      ]),
    );
    await renderAssignments();

    const vision = screen.getByLabelText('Görsel analiz');
    expect(option(vision, /Local llama/)).toBeDisabled();
    expect(option(vision, /Local llama/)).toHaveTextContent('“vision destekli” işaretli değil');
    expect(option(vision, /Local llava/)).toBeEnabled();
  });

  it('always offers local whisper for transcription and shows the saved choice', async () => {
    vi.mocked(commands.getTaskAssignments).mockResolvedValue(
      ok([{ task: 'summary', profileId: 'p-deepseek' }]),
    );
    await renderAssignments();

    const transcription = screen.getByLabelText('Transkripsiyon');
    expect(option(transcription, 'Yerel whisper (varsayılan)')).toBeInTheDocument();
    expect(transcription).toHaveValue('');
    expect(screen.getByLabelText('Özet')).toHaveValue('p-deepseek');
    expect(option(screen.getByLabelText('Özet'), 'Atanmadı')).toBeInTheDocument();
  });

  it('assigns a profile and clears the assignment again', async () => {
    const user = userEvent.setup();
    await renderAssignments();

    await user.selectOptions(screen.getByLabelText('Özet'), 'p-deepseek');
    await waitFor(() =>
      expect(commands.setTaskAssignment).toHaveBeenCalledWith('summary', 'p-deepseek'),
    );
    expect(await screen.findByText('Özet ataması kaydedildi.')).toBeInTheDocument();

    vi.mocked(commands.getTaskAssignments).mockResolvedValue(
      ok([{ task: 'transcription', profileId: 'p-openai' }]),
    );
    await user.selectOptions(screen.getByLabelText('Transkripsiyon'), 'p-openai');
    await waitFor(() =>
      expect(commands.setTaskAssignment).toHaveBeenCalledWith('transcription', 'p-openai'),
    );
    await waitFor(() => expect(screen.getByLabelText('Transkripsiyon')).toHaveValue('p-openai'));

    await user.selectOptions(screen.getByLabelText('Transkripsiyon'), '');
    await waitFor(() => expect(commands.clearTaskAssignment).toHaveBeenCalledWith('transcription'));
  });

  it('keeps the select enabled and focused while saving, and sends quick changes in order', async () => {
    const user = userEvent.setup();
    const sent: string[] = [];
    const finishers: Array<() => void> = [];
    vi.mocked(commands.setTaskAssignment).mockImplementation(((task: string, profileId: string) => {
      sent.push(profileId);
      return new Promise((resolve) => {
        finishers.push(() => resolve(ok({ task, profileId })));
      });
    }) as never);
    await renderAssignments();
    const summary = screen.getByLabelText('Özet');
    summary.focus();

    await user.selectOptions(summary, 'p-openai');
    expect(summary).toBeEnabled();
    expect(summary).toHaveFocus();
    expect(summary).toHaveAttribute('aria-busy', 'true');

    await user.selectOptions(summary, 'p-deepseek');
    expect(summary).toHaveFocus();
    expect(summary).toHaveValue('p-deepseek');
    // The second change waits for the first one to be applied.
    expect(sent).toEqual(['p-openai']);

    finishers[0]?.();
    await waitFor(() => expect(sent).toEqual(['p-openai', 'p-deepseek']));
    vi.mocked(commands.getTaskAssignments).mockResolvedValue(
      ok([{ task: 'summary', profileId: 'p-deepseek' }]),
    );
    finishers[1]?.();

    await waitFor(() => expect(summary).toHaveAttribute('aria-busy', 'false'));
    expect(summary).toHaveValue('p-deepseek');
    expect(summary).toHaveFocus();
  });

  it('reports a refused assignment by its error code', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.setTaskAssignment).mockResolvedValue(fail('capability_missing') as never);
    await renderAssignments();

    await user.selectOptions(screen.getByLabelText('Özet'), 'p-openai');

    expect(await screen.findByRole('alert')).toHaveTextContent('Bu sağlayıcı bu işi yapamıyor.');
  });

  it('warns when the assigned profile lost the capability', async () => {
    vi.mocked(commands.getTaskAssignments).mockResolvedValue(
      ok([{ task: 'vision', profileId: 'p-ollama' }]),
    );
    await renderAssignments();

    expect(screen.getByLabelText('Görsel analiz')).toHaveAccessibleDescription(
      /Atanan profil bu işi artık yapamıyor/,
    );
  });

  it('offers a retry when the assignments cannot be loaded', async () => {
    const user = userEvent.setup();
    vi.mocked(commands.getTaskAssignments).mockResolvedValueOnce(fail('internal') as never);
    renderWithProviders(<TaskAssignments />);

    expect(await screen.findByRole('alert')).toHaveTextContent('Görev atamaları yüklenemedi.');
    await user.click(screen.getByRole('button', { name: 'Yeniden dene' }));

    expect(await screen.findByLabelText('Transkripsiyon')).toBeInTheDocument();
  });
});

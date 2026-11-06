import { screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '../../i18n';
import { commands } from '../../ipc/bindings';
import { ok, renderWithProviders } from '../../test-utils';
import { daysUntil, deprecationFor } from './deprecation';
import { kindInfos, openAiProfile } from './fixtures';
import { ProfilesList } from './ProfilesList';
import { TaskAssignments } from './TaskAssignments';

vi.mock('../../ipc/bindings', () => ({
  commands: {
    listProviderProfiles: vi.fn(),
    listProviderKinds: vi.fn(),
    getTaskAssignments: vi.fn(),
    setTaskAssignment: vi.fn(),
    clearTaskAssignment: vi.fn(),
    deleteProviderProfile: vi.fn(),
    testProvider: vi.fn(),
    secretStoreStatus: vi.fn(),
    listSecrets: vi.fn(),
  },
}));

function withDeprecation(deprecated: {
  model: string;
  shutdownDate: string;
  replacement: string | null;
}) {
  return kindInfos.map((info) =>
    info.kind === 'openai' ? { ...info, deprecatedModels: [deprecated] } : info,
  );
}

describe('daysUntil', () => {
  it('counts whole calendar days, negative once passed', () => {
    const today = new Date(2026, 10, 6, 23, 30);
    expect(daysUntil('2026-11-06', today)).toBe(0);
    expect(daysUntil('2026-11-07', today)).toBe(1);
    expect(daysUntil('2027-02-24', today)).toBe(110);
    expect(daysUntil('2026-11-01', today)).toBe(-5);
    expect(daysUntil('soon', today)).toBeNull();
  });
});

describe('deprecationFor', () => {
  const kinds = withDeprecation({
    model: 'gpt-4o',
    shutdownDate: '2099-01-31',
    replacement: 'gpt-5',
  });

  it('matches the exact model of the right provider only', () => {
    expect(deprecationFor(kinds, 'openai', 'gpt-4o')?.replacement).toBe('gpt-5');
    expect(deprecationFor(kinds, 'openai', ' gpt-4o ')).not.toBeNull();
    expect(deprecationFor(kinds, 'openai', 'gpt-4o-mini')).toBeNull();
    expect(deprecationFor(kinds, 'anthropic', 'gpt-4o')).toBeNull();
    expect(deprecationFor(undefined, 'openai', 'gpt-4o')).toBeNull();
  });
});

describe('profile list deprecation badge', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
    vi.mocked(commands.listProviderProfiles).mockResolvedValue(ok([openAiProfile]));
    vi.mocked(commands.secretStoreStatus).mockResolvedValue({
      backend: 'keyring',
      available: true,
      locked: false,
      masterPasswordSet: false,
    });
    vi.mocked(commands.listSecrets).mockResolvedValue(ok([]));
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it('warns with the shutdown date and the replacement', async () => {
    vi.mocked(commands.listProviderKinds).mockResolvedValue(
      withDeprecation({ model: 'gpt-4o', shutdownDate: '2099-01-31', replacement: 'gpt-5' }),
    );
    renderWithProviders(<ProfilesList />);

    expect(await screen.findByText(/Kullanımdan kalkıyor: 31 Ocak 2099/)).toBeInTheDocument();
    expect(screen.getByText('Önerilen: gpt-5')).toBeInTheDocument();
  });

  it('says so when the model has already been switched off', async () => {
    vi.mocked(commands.listProviderKinds).mockResolvedValue(
      withDeprecation({ model: 'gpt-4o', shutdownDate: '2020-01-31', replacement: null }),
    );
    renderWithProviders(<ProfilesList />);

    expect(await screen.findByText('Kapandı: 31 Ocak 2020')).toBeInTheDocument();
    expect(screen.queryByText(/Önerilen/)).not.toBeInTheDocument();
  });

  it('shows nothing for a model that is not closing, or when the facts cannot be loaded', async () => {
    vi.mocked(commands.listProviderKinds).mockRejectedValue(new Error('offline'));
    renderWithProviders(<ProfilesList />);

    expect(await screen.findByText('gpt-4o')).toBeInTheDocument();
    expect(screen.queryByText(/Kullanımdan kalkıyor|Kapandı/)).not.toBeInTheDocument();
  });
});

describe('task assignment deprecation badge', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
    vi.mocked(commands.listProviderProfiles).mockResolvedValue(
      ok([{ ...openAiProfile, model: 'whisper-1' }]),
    );
    vi.mocked(commands.getTaskAssignments).mockResolvedValue(
      ok([{ task: 'transcription', profileId: openAiProfile.id }]),
    );
    vi.mocked(commands.listProviderKinds).mockResolvedValue(
      withDeprecation({
        model: 'whisper-1',
        shutdownDate: '2099-02-26',
        replacement: 'gpt-transcribe',
      }),
    );
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it('warns under the task whose selected profile uses a closing model', async () => {
    renderWithProviders(<TaskAssignments />);

    const select = await screen.findByLabelText('Transkripsiyon');
    expect(select).toHaveDisplayValue(/OpenAI work/);
    const badge = await screen.findByText(/Kullanımdan kalkıyor: 26 Şubat 2099/);
    expect(select.closest('.assign-cell')).toContainElement(badge);
    expect(screen.getByText('Önerilen: gpt-transcribe')).toBeInTheDocument();
    // Only the transcription task points at that profile.
    expect(screen.getAllByText(/Kullanımdan kalkıyor/)).toHaveLength(1);
  });
});

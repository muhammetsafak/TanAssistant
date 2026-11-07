import { act, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import i18n from '../../i18n';
import { commands, type DisplayThumbnailUnavailableReasonDto } from '../../ipc/bindings';
import { fail, ok, renderWithProviders } from '../../test-utils';
import { THUMBNAIL_REFRESH_MS } from './api';
import { DisplayPicker } from './DisplayPicker';

vi.mock('../../ipc/bindings', async () => {
  const { stubEvents } = await import('../../test-events');
  return { commands: { displayThumbnail: vi.fn() }, events: stubEvents() };
});

const displays = [
  { id: 'd1', name: 'Built-in display', width: 3024, height: 1964, isPrimary: true },
  { id: 'd2', name: 'DELL U2720Q', width: 3840, height: 2160, isPrimary: false },
];

const image = (dataUrl: string) =>
  ok({ available: true, dataUrl, width: 320, height: 180, unavailableReason: null });

const unavailable = (reason: DisplayThumbnailUnavailableReasonDto) =>
  ok({ available: false, dataUrl: null, width: null, height: null, unavailableReason: reason });

function Harness({ onChange }: { onChange?: (id: string) => void }) {
  const [value, setValue] = useState('');
  return (
    <DisplayPicker
      displays={displays}
      value={value}
      onChange={(id) => {
        setValue(id);
        onChange?.(id);
      }}
      loading={false}
      failed={false}
    />
  );
}

describe('DisplayPicker', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
  });

  afterEach(() => {
    vi.clearAllMocks();
    vi.useRealTimers();
  });

  it('asks for no preview while the list is closed', async () => {
    renderWithProviders(<Harness />);

    expect(screen.getByRole('button', { name: /Ekran/ })).toHaveAttribute('aria-expanded', 'false');
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(commands.displayThumbnail).not.toHaveBeenCalled();
  });

  it('shows a placeholder while loading, then the preview with the display name as alt text', async () => {
    let resolveFirst: (value: ReturnType<typeof image>) => void = () => undefined;
    vi.mocked(commands.displayThumbnail).mockImplementation((id) =>
      id === 'd1'
        ? new Promise((resolve) => {
            resolveFirst = resolve as typeof resolveFirst;
          })
        : Promise.resolve(image('data:image/png;base64,BBBB')),
    );
    const user = userEvent.setup();
    renderWithProviders(<Harness />);

    await user.click(screen.getByRole('button', { name: /Ekran/ }));

    expect(commands.displayThumbnail).toHaveBeenCalledWith('d1');
    expect(commands.displayThumbnail).toHaveBeenCalledWith('d2');
    expect(await screen.findByAltText('DELL U2720Q')).toHaveAttribute(
      'src',
      'data:image/png;base64,BBBB',
    );
    expect(screen.getByText('Önizleme yükleniyor')).toBeInTheDocument();
    expect(screen.queryByAltText('Built-in display')).not.toBeInTheDocument();

    act(() => resolveFirst(image('data:image/png;base64,AAAA')));
    expect(await screen.findByAltText('Built-in display')).toHaveAttribute(
      'src',
      'data:image/png;base64,AAAA',
    );
    expect(screen.queryByText('Önizleme yükleniyor')).not.toBeInTheDocument();
  });

  it('refreshes the previews every few seconds while open and stops when closed', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(commands.displayThumbnail).mockImplementation(() =>
      Promise.resolve(image('data:image/png;base64,AAAA')),
    );
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderWithProviders(<Harness />);

    await user.click(screen.getByRole('button', { name: /Ekran/ }));
    await screen.findByAltText('DELL U2720Q');
    expect(commands.displayThumbnail).toHaveBeenCalledTimes(2);

    await act(() => vi.advanceTimersByTimeAsync(THUMBNAIL_REFRESH_MS + 100));
    await waitFor(() => expect(commands.displayThumbnail).toHaveBeenCalledTimes(4));

    await user.click(screen.getByRole('button', { name: /Ekran/ }));
    const callsWhenClosed = vi.mocked(commands.displayThumbnail).mock.calls.length;
    await act(() => vi.advanceTimersByTimeAsync(THUMBNAIL_REFRESH_MS * 3));
    expect(commands.displayThumbnail).toHaveBeenCalledTimes(callsWhenClosed);
  });

  it('explains why a display has no preview and still lets it be chosen', async () => {
    vi.mocked(commands.displayThumbnail).mockImplementation((id) =>
      Promise.resolve(
        id === 'd1' ? unavailable('permission_denied') : unavailable('wayland_unsupported'),
      ),
    );
    const onChange = vi.fn();
    const user = userEvent.setup();
    renderWithProviders(<Harness onChange={onChange} />);

    await user.click(screen.getByRole('button', { name: /Ekran/ }));

    expect(await screen.findByText('Ekran Kaydı izni gerekli')).toBeInTheDocument();
    expect(screen.getByText('Paylaşım sırasında seçilecek')).toBeInTheDocument();
    expect(screen.queryByRole('img')).not.toBeInTheDocument();

    const radio = screen.getByRole('radio', { name: 'DELL U2720Q' });
    expect(radio).toHaveAccessibleDescription(/3840×2160/);
    expect(radio).toHaveAccessibleDescription(/Paylaşım sırasında seçilecek/);
    await user.click(radio);
    expect(onChange).toHaveBeenCalledWith('d2');
    expect(screen.getByRole('button', { name: /Ekran.*DELL U2720Q/ })).toBeInTheDocument();
  });

  it('falls back to a plain hint when the preview command fails', async () => {
    vi.mocked(commands.displayThumbnail).mockResolvedValue(fail('internal') as never);
    const user = userEvent.setup();
    renderWithProviders(<Harness />);

    await user.click(screen.getByRole('button', { name: /Ekran/ }));

    expect(await screen.findAllByText('Önizleme alınamadı')).toHaveLength(2);
  });

  it('keeps the default screen as the first choice', async () => {
    vi.mocked(commands.displayThumbnail).mockResolvedValue(unavailable('unsupported'));
    const user = userEvent.setup();
    renderWithProviders(<Harness />);

    await user.click(screen.getByRole('button', { name: /Ekran/ }));

    const radios = screen.getAllByRole('radio');
    expect(radios[0]).toBeChecked();
    expect(radios[0]).toHaveAccessibleName('Varsayılan ekran');
    expect(radios).toHaveLength(3);
  });

  it('has text for every reason the backend can give, and a generic one for an unknown reason', async () => {
    const reasons: Record<string, string> = {
      permission_denied: 'Ekran Kaydı izni gerekli',
      wayland_unsupported: 'Paylaşım sırasında seçilecek',
      display_not_found: 'Ekran artık bağlı değil',
      unsupported: 'Bu platformda önizleme yok',
      failed: 'Önizleme alınamadı',
      brand_new_reason: 'Önizleme alınamadı',
    };
    const user = userEvent.setup();
    for (const [reason, text] of Object.entries(reasons)) {
      vi.mocked(commands.displayThumbnail).mockResolvedValue(
        unavailable(reason as DisplayThumbnailUnavailableReasonDto),
      );
      const { unmount } = renderWithProviders(<Harness />);
      await user.click(screen.getByRole('button', { name: /Ekran/ }));
      expect((await screen.findAllByText(text)).length).toBeGreaterThan(0);
      expect(screen.queryByText(/recording\.thumbnail/)).not.toBeInTheDocument();
      unmount();
    }
  });

  it.each(['wayland_unsupported', 'unsupported'] as const)(
    'stops refreshing a preview that is %s for good',
    async (reason) => {
      vi.useFakeTimers({ shouldAdvanceTime: true });
      vi.mocked(commands.displayThumbnail).mockResolvedValue(unavailable(reason));
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
      renderWithProviders(<Harness />);

      await user.click(screen.getByRole('button', { name: /Ekran/ }));
      await waitFor(() => expect(commands.displayThumbnail).toHaveBeenCalledTimes(2));
      await act(() => vi.advanceTimersByTimeAsync(THUMBNAIL_REFRESH_MS * 2 + 100));

      expect(commands.displayThumbnail).toHaveBeenCalledTimes(2);
    },
  );

  it('keeps refreshing a preview that failed only this time', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(commands.displayThumbnail).mockResolvedValue(unavailable('failed'));
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderWithProviders(<Harness />);

    await user.click(screen.getByRole('button', { name: /Ekran/ }));
    await waitFor(() => expect(commands.displayThumbnail).toHaveBeenCalledTimes(2));
    await act(() => vi.advanceTimersByTimeAsync(THUMBNAIL_REFRESH_MS + 100));

    await waitFor(() => expect(commands.displayThumbnail).toHaveBeenCalledTimes(4));
  });

  it('closes the list with Escape and returns the focus to its button', async () => {
    vi.mocked(commands.displayThumbnail).mockResolvedValue(unavailable('unsupported'));
    const user = userEvent.setup();
    renderWithProviders(<Harness />);
    const toggle = screen.getByRole('button', { name: /Ekran/ });

    await user.click(toggle);
    await user.click(screen.getByRole('radio', { name: 'DELL U2720Q' }));
    expect(screen.getByRole('radiogroup')).toBeInTheDocument();
    await user.keyboard('{Escape}');

    expect(screen.queryByRole('radiogroup')).not.toBeInTheDocument();
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    expect(toggle).toHaveFocus();
  });
});

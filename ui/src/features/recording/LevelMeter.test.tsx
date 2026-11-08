import { screen } from '@testing-library/react';
import { beforeEach, describe, expect, it } from 'vitest';

import i18n from '../../i18n';
import { renderWithProviders } from '../../test-utils';
import { LevelMeter } from './LevelMeter';

function lit(container: HTMLElement) {
  return container.querySelectorAll('.level__bar--on').length;
}

describe('LevelMeter', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
  });

  it('lights as many of the 16 bars as the level asks for and exposes a meter', () => {
    const { container } = renderWithProviders(
      <LevelMeter name="Ben" source="Mikrofon" tone="me" level={0.25} />,
    );

    expect(container.querySelectorAll('.level__bar')).toHaveLength(16);
    expect(lit(container)).toBe(8);
    const meter = screen.getByRole('meter', { name: 'Mikrofon seviyesi' });
    expect(meter).toHaveAttribute('value', '0.5');
  });

  it('says "sinyal yok" when the source delivers nothing', () => {
    const { container } = renderWithProviders(
      <LevelMeter name="Karşı taraf" source="Sistem sesi" tone="them" level={null} />,
    );

    expect(lit(container)).toBe(0);
    expect(screen.getByText('sinyal yok')).toBeInTheDocument();
  });

  it('is empty and says so while paused, even when a level arrives', () => {
    const { container } = renderWithProviders(
      <LevelMeter name="Ben" source="Mikrofon" tone="me" level={0.9} mode="paused" />,
    );

    expect(lit(container)).toBe(0);
    expect(screen.getByText('duraklatıldı')).toBeInTheDocument();
    expect(screen.getByRole('meter', { name: 'Mikrofon seviyesi' })).toHaveAttribute('value', '0');
  });

  it('is a decorative, dimmed preview while idle', () => {
    const { container } = renderWithProviders(
      <LevelMeter name="Ben" source="Mikrofon" tone="me" level={0.9} mode="idle" />,
    );

    expect(lit(container)).toBe(0);
    expect(screen.queryByRole('meter')).not.toBeInTheDocument();
    expect(container.querySelector('.level__bars')).toHaveAttribute('aria-hidden', 'true');
    // The class that dims the bars (opacity in recording.css) is on the meter.
    expect(container.querySelector('.level')).toHaveClass('level--idle');
  });
});

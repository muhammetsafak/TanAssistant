import { render, screen } from '@testing-library/react';

import { Chip } from './Chip';

describe('Chip', () => {
  it.each(['live', 'paused', 'processing', 'ready'] as const)(
    'shows a decorative icon for %s',
    (kind) => {
      render(<Chip kind={kind}>Status</Chip>);
      const chip = screen.getByText('Status');
      expect(chip).toHaveClass('chip', `chip--${kind}`);
      expect(chip.querySelector('svg')).toHaveAttribute('aria-hidden', 'true');
    },
  );
});

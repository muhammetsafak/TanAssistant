import { render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it } from 'vitest';

import i18n from '../../i18n';
import { TanIllustration, type TanPose } from './TanIllustration';

const POSES: TanPose[] = ['waving', 'sitting-desk', 'focused', 'approving', 'thinking', 'happy'];

describe('TanIllustration', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('tr');
  });

  it('describes every pose in Turkish and English', async () => {
    for (const pose of POSES) {
      const { unmount } = render(<TanIllustration pose={pose} />);
      const turkish = screen.getByRole('img').getAttribute('alt');
      unmount();
      await i18n.changeLanguage('en');
      const english = render(<TanIllustration pose={pose} />);
      const description = screen.getByRole('img').getAttribute('alt');
      english.unmount();
      await i18n.changeLanguage('tr');

      expect(turkish).toBeTruthy();
      expect(turkish).not.toMatch(/^tan\.alt\./);
      expect(description).toBeTruthy();
      expect(description).not.toMatch(/^tan\.alt\./);
      expect(description).not.toBe(turkish);
    }
  });

  it('reserves the space of the picture through its width and height', () => {
    render(<TanIllustration pose="sitting-desk" width={110} />);
    const image = screen.getByRole('img');
    expect(image).toHaveAttribute('width', '110');
    expect(image).toHaveAttribute('height', '113');
  });

  it('takes a custom description and can be decorative', () => {
    const { rerender } = render(<TanIllustration pose="thinking" alt="Custom" />);
    expect(screen.getByRole('img', { name: 'Custom' })).toBeInTheDocument();

    rerender(<TanIllustration pose="thinking" decorative />);
    expect(screen.queryByRole('img')).not.toBeInTheDocument();
    expect(document.querySelector('img')).toHaveAttribute('alt', '');
  });
});

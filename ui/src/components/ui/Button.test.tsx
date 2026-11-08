import { render, screen } from '@testing-library/react';

import { Button } from './Button';

describe('Button', () => {
  it('is a non-submitting secondary button by default', () => {
    render(<Button>Save</Button>);
    const button = screen.getByRole('button', { name: 'Save' });
    expect(button).toHaveAttribute('type', 'button');
    expect(button).toHaveClass('btn', 'btn--secondary');
  });

  it.each(['primary', 'live', 'ghost'] as const)('applies the %s variant class', (variant) => {
    render(<Button variant={variant}>Go</Button>);
    expect(screen.getByRole('button', { name: 'Go' })).toHaveClass(`btn--${variant}`);
  });

  it('shows a decorative warning icon on destructive buttons', () => {
    render(<Button variant="danger">Delete</Button>);
    const button = screen.getByRole('button', { name: 'Delete' });
    const icon = button.querySelector('svg');
    expect(icon).not.toBeNull();
    expect(icon).toHaveAttribute('aria-hidden', 'true');
  });

  it('keeps an icon-only button name from aria-label and adds no icon of its own', () => {
    render(
      <Button variant="danger" iconOnly aria-label="Remove">
        <span>x</span>
      </Button>,
    );
    const button = screen.getByRole('button', { name: 'Remove' });
    expect(button).toHaveClass('btn--icon');
    expect(button.querySelector('svg')).toBeNull();
  });

  it('passes the small size and extra class names through', () => {
    render(
      <Button size="sm" className="extra">
        Small
      </Button>,
    );
    expect(screen.getByRole('button', { name: 'Small' })).toHaveClass('btn--sm', 'extra');
  });

  it('requires an accessible name for icon-only buttons at the type level', () => {
    // Never invoked: the point is that `tsc --noEmit` rejects the unnamed form below.
    const unnamed = () => (
      // @ts-expect-error iconOnly without aria-label must not compile
      <Button iconOnly>x</Button>
    );
    expect(typeof unnamed).toBe('function');
  });
});

import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import { renderWithProviders } from '../../test-utils';
import { useToast } from './toast-context';

function Trigger() {
  const toast = useToast();
  return (
    <>
      <button onClick={() => toast.info('Short dropouts happened.')}>info</button>
      <button onClick={() => toast.success('Saved.')}>success</button>
      <button onClick={() => toast.errorText('It failed.')}>error</button>
    </>
  );
}

describe('Toast', () => {
  it('shows an info toast as a calm status message, not an alert', async () => {
    const user = userEvent.setup();
    renderWithProviders(<Trigger />);

    await user.click(screen.getByRole('button', { name: 'info' }));

    const toast = screen.getByRole('status');
    expect(toast).toHaveTextContent('Short dropouts happened.');
    expect(toast).toHaveClass('toast--info');
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('keeps the success and error variants as they were', async () => {
    const user = userEvent.setup();
    renderWithProviders(<Trigger />);

    await user.click(screen.getByRole('button', { name: 'success' }));
    await user.click(screen.getByRole('button', { name: 'error' }));

    expect(screen.getByRole('status')).toHaveClass('toast--success');
    expect(screen.getByRole('alert')).toHaveClass('toast--error');
  });
});

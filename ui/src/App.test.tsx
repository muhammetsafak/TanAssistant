import { render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import App from './App';
import { commands } from './ipc/bindings';

vi.mock('./ipc/bindings', () => ({
  commands: { appVersion: vi.fn() },
}));

describe('App', () => {
  it('shows the version returned by the app_version command', async () => {
    vi.mocked(commands.appVersion).mockResolvedValue('1.2.3');

    render(<App />);

    expect(await screen.findByText('Version 1.2.3')).toBeInTheDocument();
  });

  it('shows an error when the command fails', async () => {
    vi.mocked(commands.appVersion).mockRejectedValue(new Error('boom'));

    render(<App />);

    expect(await screen.findByRole('alert')).toBeInTheDocument();
  });
});

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, type RenderResult } from '@testing-library/react';
import type { ReactElement } from 'react';
import { MemoryRouter } from 'react-router';

import { ToastProvider } from './components/ui/Toast';
import { ThemeProvider } from './theme/ThemeProvider';

import './i18n';

/** Builds the wire shape the generated `commands` resolve with on success. */
export function ok<T>(data: T): { status: 'ok'; data: T } {
  return { status: 'ok', data };
}

/** Builds the wire shape the generated `commands` resolve with on failure. */
export function fail(
  code: string,
  message = 'backend message',
): { status: 'error'; error: unknown } {
  return { status: 'error', error: { code, message } };
}

/** Renders `ui` inside every provider the real app has, with a fresh query cache. */
export function renderWithProviders(
  ui: ReactElement,
  route = '/',
): RenderResult & { queryClient: QueryClient } {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 }, mutations: { retry: false } },
  });
  const result = render(
    <QueryClientProvider client={queryClient}>
      <ThemeProvider>
        <ToastProvider>
          <MemoryRouter initialEntries={[route]}>{ui}</MemoryRouter>
        </ToastProvider>
      </ThemeProvider>
    </QueryClientProvider>,
  );
  return { ...result, queryClient };
}

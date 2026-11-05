import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { useState } from 'react';
import { HashRouter } from 'react-router';

import { ToastProvider } from './components/ui/Toast';
import { AppRoutes } from './router';
import { ThemeProvider } from './theme/ThemeProvider';

function createQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      // IPC calls are local: a failure is real, so surface it instead of retrying silently.
      queries: { retry: false, refetchOnWindowFocus: false },
      mutations: { retry: false },
    },
  });
}

export default function App() {
  const [queryClient] = useState(createQueryClient);

  return (
    <QueryClientProvider client={queryClient}>
      <ThemeProvider>
        <ToastProvider>
          <HashRouter>
            <AppRoutes />
          </HashRouter>
        </ToastProvider>
      </ThemeProvider>
    </QueryClientProvider>
  );
}

import React from 'react';
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from 'react-router';
import '../theme/tailwind.css';
import { applyEmbeddedPublicPath, dashboardRouterBasename } from './embedBasename.ts';
import { dashboardRouterForPath, ScopedEventsProvider } from './runtime.tsx';

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // Server state is revision-monotone; SSE invalidations drive refresh.
      staleTime: 15_000,
      retry: 1,
    },
  },
});

const rootEl = document.getElementById('root');
if (!rootEl) throw new Error('missing #root');

// Theme boot: explicit user choice wins; otherwise follow the OS.
const storedTheme = localStorage.getItem('td-theme');
if (storedTheme === 'light' || storedTheme === 'dark') {
  document.documentElement.dataset['theme'] = storedTheme;
}

const embedBasename = dashboardRouterBasename(window.location.pathname);
applyEmbeddedPublicPath(embedBasename);
const router = dashboardRouterForPath(window.location.pathname);

createRoot(rootEl).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>
      <ScopedEventsProvider>
        <RouterProvider router={router} />
      </ScopedEventsProvider>
    </QueryClientProvider>
  </React.StrictMode>,
);

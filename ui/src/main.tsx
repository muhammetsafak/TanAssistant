import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import App from './App';
import './i18n';
import { applyTheme, readStoredTheme } from './theme/theme-storage';

import './theme/tokens.css';
import './theme/base.css';

// Apply the stored theme before the first paint to avoid a flash of the wrong one.
applyTheme(readStoredTheme());

const container = document.getElementById('root');
if (!container) {
  throw new Error('Root element #root not found');
}

createRoot(container).render(
  <StrictMode>
    <App />
  </StrictMode>,
);

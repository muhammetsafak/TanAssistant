import { useCallback, useMemo, useState, type ReactNode } from 'react';

import { ThemeContext } from './theme-context';
import { applyTheme, readStoredTheme, storeTheme, type ThemePreference } from './theme-storage';

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [theme, setThemeState] = useState<ThemePreference>(readStoredTheme);

  const setTheme = useCallback((next: ThemePreference) => {
    applyTheme(next);
    storeTheme(next);
    setThemeState(next);
  }, []);

  const value = useMemo(() => ({ theme, setTheme }), [theme, setTheme]);

  return <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>;
}

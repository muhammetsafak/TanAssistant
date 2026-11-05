import { useEffect, useState } from 'react';

import { commands } from './ipc/bindings';

export default function App() {
  const [version, setVersion] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    commands
      .appVersion()
      .then((value) => {
        if (!cancelled) setVersion(value);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <main>
      <h1>Tan</h1>
      {failed ? (
        <p role="alert">Could not read the application version.</p>
      ) : (
        <p data-testid="app-version">{version === null ? 'Loading…' : `Version ${version}`}</p>
      )}
    </main>
  );
}

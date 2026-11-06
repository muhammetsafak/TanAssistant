import { useEffect, useRef } from 'react';

interface EventSource<T> {
  listen: (callback: (event: { payload: T }) => void) => Promise<() => void>;
}

/**
 * Subscribes to a generated backend event for as long as the component is mounted. The handler
 * may change on every render without re-subscribing. A failed subscription (for example in a
 * plain browser, without the Tauri runtime) is ignored: the screen then simply gets no live
 * updates.
 */
export function useTauriEvent<T>(source: EventSource<T>, handler: (payload: T) => void): void {
  const handlerRef = useRef(handler);
  useEffect(() => {
    handlerRef.current = handler;
  });

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    Promise.resolve()
      .then(() => source.listen((event) => handlerRef.current(event.payload)))
      .then((stop) => {
        if (disposed) {
          stop();
        } else {
          unlisten = stop;
        }
      })
      .catch(() => undefined);
    return () => {
      disposed = true;
      unlisten?.();
    };
    // `source` is a module-level constant of the generated bindings.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
}

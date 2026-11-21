import { useEffect } from 'react';

let prefetched = false;
export function usePanelPrefetch(ready: boolean, loaders: readonly (() => void)[]) {
  useEffect(() => {
    const connection = (navigator as Navigator & { connection?: { saveData?: boolean } }).connection;
    if (!ready || prefetched || connection?.saveData) return;
    let cancelIdle: (() => void) | undefined;
    let frame = requestAnimationFrame(() => {
      frame = requestAnimationFrame(() => {
        if (window.requestIdleCallback) {
          const idle = window.requestIdleCallback(prefetch, { timeout: 1500 });
          cancelIdle = () => window.cancelIdleCallback(idle);
        } else {
          const timer = window.setTimeout(prefetch, 0);
          cancelIdle = () => window.clearTimeout(timer);
        }
      });
    });
    function prefetch() {
      prefetched = true;
      for (const load of loaders) load();
    }
    return () => { cancelAnimationFrame(frame); cancelIdle?.(); };
  }, [ready, loaders]);
}

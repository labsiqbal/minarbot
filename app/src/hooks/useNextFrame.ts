import { useCallback, useEffect, useRef } from 'react';

import { createFrameQueue } from './frame-queue';
export { createFrameQueue } from './frame-queue';

export function useNextFrame() {
  const queue = useRef<ReturnType<typeof createFrameQueue> | null>(null);
  useEffect(() => {
    const next = createFrameQueue(); queue.current = next;
    return () => { next.cancel(); queue.current = null; };
  }, []);
  return useCallback((callback: () => void) => queue.current?.schedule(callback), []);
}

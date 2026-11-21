import { useEffect, useMemo, useState, useSyncExternalStore, type DependencyList, type SetStateAction } from 'react';
import { queryCache } from './queryCache.ts';
import { usePolling } from './usePolling.ts';
import type { Poll, PollingOptions } from './polling.ts';

export function useCachedData<T>(path: string) {
  const start = useMemo(() => ({ path, read: queryCache.reads() }), [path]);
  const version = useQueryVersion();
  const entry = useSyncExternalStore(queryCache.subscribe, () => queryCache.peek(path));
  useEffect(() => queryCache.touch(path), [path]);
  return {
    data: (entry?.data as T | undefined) ?? null,
    fresh: !!entry && entry.version === version && start.path === path && entry.read > start.read,
  };
}

export function useQueryVersion() {
  return useSyncExternalStore(queryCache.subscribe, queryCache.version);
}

export function useQueryPolling(fn: Poll, options: PollingOptions, dependencies: DependencyList = []) {
  const version = useQueryVersion();
  usePolling(fn, options, [...dependencies, version]);
}

export function useCachedQuery<T>(request: <U>(path: string) => Promise<U>, path: string, enabled = true) {
  const { data, fresh } = useCachedData<T>(path);
  const version = useQueryVersion();
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(true);
  const [revision, setRevision] = useState(0);

  useEffect(() => {
    if (!enabled) return;
    let active = true;
    setError(null);
    setBusy(true);
    void queryCache.refresh(path, () => request<T>(path)).catch(reason => {
      if (active) setError(reason);
    }).finally(() => {
      if (active) setBusy(false);
    });
    return () => { active = false; };
  }, [enabled, path, request, revision, version]);

  function setData(next: SetStateAction<T | null>) {
    const current = (queryCache.peek(path)?.data as T | undefined) ?? null;
    queryCache.set(path, typeof next === 'function' ? (next as (value: T | null) => T | null)(current) : next);
  }

  return { data, fresh, setData, error, busy, reload: () => setRevision(value => value + 1) };
}

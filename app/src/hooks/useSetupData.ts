import { useCachedQuery } from './useCachedQuery';

export function useSetupData<T>(request: <U>(path: string) => Promise<U>, path: string, enabled = true) {
  const query = useCachedQuery<T>(request, path, enabled);
  return { ...query, error: query.error !== null };
}

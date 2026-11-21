import { useCachedQuery } from './useCachedQuery';

export const webSearchSettingsPath = '/api/settings/web-search';
export type WebSearchConfig = { url: string | null; managed: boolean };

export function useWebSearchSettings(request: <T>(path: string) => Promise<T>) {
  const { data: settings, setData: setSettings, error } = useCachedQuery<WebSearchConfig>(request, webSearchSettingsPath);
  return { settings, setSettings, loadError: error ? 'Web search settings could not load. Reopen General to retry.' : '' };
}

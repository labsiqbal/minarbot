export type SettingsSection = 'general' | 'appearance' | 'providers' | 'engines' | 'skills' | 'connected-apps' | 'hub' | 'mcp' | 'machines' | 'devices' | 'computer' | 'updates' | 'usage' | 'data' | 'backup' | 'mcp-server';
export type ResultTarget = { view: 'file' | 'url' | 'computer'; path: string; name: string; agent?: string };
export type AppRoute = { result?: ResultTarget; notFound?: boolean; agent?: string; thread?: string; room?: string; project?: string; machine?: string; remoteAgent?: string; view?: 'routines' | 'workflows'; settings?: SettingsSection };
const settings = new Set(['general', 'appearance', 'providers', 'engines', 'skills', 'connected-apps', 'hub', 'mcp', 'machines', 'devices', 'computer', 'updates', 'usage', 'data', 'backup', 'mcp-server']);
const id = '[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}';

function parseBaseRoute(location: string): AppRoute {
  const [path, query = ''] = location.split('?');
  let match;
  if (path === '/') return {};
  if ((match = /^\/settings\/([^/]+)$/.exec(path)) && settings.has(match[1])) {
    const from = new URLSearchParams(query).get('from') ?? '/';
    return { ...(!from.startsWith('/settings/') && !parseRoute(from).notFound ? parseRoute(from) : {}), settings: match[1] as SettingsSection };
  }
  if ((match = new RegExp(`^/agents/(${id})(?:/conversations/(${id}))?$`, 'i').exec(path))) return { agent: match[1].toLowerCase(), ...(match[2] ? { thread: match[2].toLowerCase() } : {}) };
  if ((match = new RegExp(`^/rooms/(${id})$`, 'i').exec(path))) return { room: match[1].toLowerCase() };
  if ((match = new RegExp(`^/projects/(${id})$`, 'i').exec(path))) return { project: match[1].toLowerCase() };
  if ((match = new RegExp(`^/machines/(${id})/agents/(${id})$`, 'i').exec(path))) return { machine: match[1].toLowerCase(), remoteAgent: match[2].toLowerCase() };
  if (path === '/routines' || path === '/workflows') return { view: path.slice(1) as 'routines' | 'workflows' };
  return { notFound: true };
}

function baseRoutePath(route: AppRoute): string {
  if (route.settings) {
    const from = routePath({ ...route, settings: undefined });
    return `/settings/${route.settings}${from === '/' ? '' : `?${new URLSearchParams({ from })}`}`;
  }
  if (route.view) return `/${route.view}`;
  if (route.machine && route.remoteAgent) return `/machines/${route.machine}/agents/${route.remoteAgent}`;
  if (route.project) return `/projects/${route.project}`;
  if (route.room) return `/rooms/${route.room}`;
  if (route.agent) return `/agents/${route.agent}${route.thread ? `/conversations/${route.thread}` : ''}`;
  return '/';
}

export function resultTarget(value: unknown): ResultTarget | null {
  if (!value || typeof value !== 'object') return null;
  const { view, path, name, agent } = value as Record<string, unknown>;
  if (typeof path !== 'string' || path.length > 4096 || /[\x00-\x20\x7f\\]/.test(path) || typeof name !== 'string' || name.length > 1024) return null;
  if (view === 'file' && new RegExp(`^/api/(agents|channels|threads)/${id}/panel/files/${id}$`, 'i').test(path)) return { view, path, name };
  if (view === 'url' || view === 'computer') {
    try { const url = new URL(path); if (['http:', 'https:'].includes(url.protocol) && !url.username && !url.password) return view === 'computer' ? agent === undefined || typeof agent === 'string' && new RegExp(`^${id}$`, 'i').test(agent) ? { view, path: url.href, name, ...(agent ? { agent } : {}) } : null : { view, path: url.href, name }; } catch { return null; }
  }
  return null;
}
export function parseRoute(location: string): AppRoute {
  const route = parseBaseRoute(location), params = new URLSearchParams(location.split('?')[1] ?? '');
  if (!params.has('result')) return route;
  if (params.getAll('result').length !== 1) return { ...route, notFound: true };
  try { const result = resultTarget(JSON.parse(params.get('result')!)); return result ? { ...route, result } : { ...route, notFound: true }; }
  catch { return { ...route, notFound: true }; }
}
export function routePath(route: AppRoute): string {
  const path = baseRoutePath(route);
  if (!route.result || route.settings) return path;
  return `${path}?${new URLSearchParams({ result: JSON.stringify(route.result) })}`;
}

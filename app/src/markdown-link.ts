import { decodeHTML } from 'entities';
import { parseRoute, type ResultTarget } from './navigation.ts';

export function markdownLink(value: string, scope: string, origin: string): { href: string; external: boolean; result?: ResultTarget } | null {
  value = decodeHTML(value);
  if (/[\x00-\x20\x7f\\]/.test(value)) return null;
  if (/^https?:\/\//i.test(value)) {
    try { const url = new URL(value); if (url.username || url.password) return null; if (url.origin !== origin) return { href: url.href, external: true }; value = url.pathname + url.search + url.hash; } catch { return null; }
  }
  if (value.startsWith('/') && !value.startsWith('//')) { const route = parseRoute(value); if (!route.notFound && route.result) return { href: value, external: false, result: route.result }; }
  if (value.startsWith(scope + '/files/') && /^[a-f0-9-]{36}$/i.test(value.slice((scope + '/files/').length))) return { href: value, external: false };
  if (/^\/sites\/[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(value)) return { href: value, external: true };
  return null;
}

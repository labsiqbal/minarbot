import { useEffect, useRef, useState, type SetStateAction } from 'react';
import { parseRoute, routePath, type AppRoute } from '../navigation';

const returnFocusKey = 'minarbot:appearance-return-focus';
const locationPath = () => window.location.pathname + window.location.search;

function ownedPath(route: AppRoute) {
  const path = routePath(route);
  if (!new URLSearchParams(window.location.search).getAll('ui').includes('react')) return path;
  return path + (path.includes('?') ? '&' : '?') + 'ui=react';
}

export function useAppearanceReturnFocus(ready: boolean) {
  useEffect(() => {
    if (!ready) return;
    try {
      if (sessionStorage.getItem(returnFocusKey) !== locationPath()) return;
      const trigger = document.querySelector<HTMLButtonElement>('.sidebar-settings, .sidebar-reveal');
      if (!trigger) return;
      sessionStorage.removeItem(returnFocusKey);
      trigger.focus();
    } catch { /* Storage may be disabled. */ }
  }, [ready]);
}

export function useAppRoute() {
  const [route, setRoute] = useState(() => parseRoute(locationPath()));
  const current = useRef(route);
  useEffect(() => {
    const pop = () => { current.current = parseRoute(locationPath()); setRoute(current.current); };
    window.addEventListener('popstate', pop);
    return () => window.removeEventListener('popstate', pop);
  }, []);
  const navigate = (next: SetStateAction<AppRoute>, replace = false) => {
    const value = typeof next === 'function' ? next(current.current) : next;
    const path = ownedPath(value);
    const settings = value.settings && current.current.settings !== value.settings;
    const project = !value.settings && !value.result && !!value.project && current.current.project !== value.project;
    const automation = !value.settings && !!value.view && current.current.view !== value.view;
    const conversation = !value.settings && !value.result && !value.inbox && (value.room ||
      value.agent && value.agent !== '00000000-0000-4000-8000-000000000005');
    if (path !== locationPath() && (settings || project || automation || conversation) && document.querySelector('meta[name="minarbot-appearance-client"][content="leptos"]')) {
      window.location[replace ? 'replace' : 'assign'](path);
      return;
    }
    if (path !== locationPath()) window.history[replace ? 'replaceState' : 'pushState'](null, '', path);
    current.current = value; setRoute(value);
  };
  return { route, navigate };
}

export function useRouteFallback(invalid: boolean, fallback: AppRoute, navigate: (route: AppRoute, replace: boolean) => void) {
  useEffect(() => { if (invalid && ownedPath(fallback) !== locationPath()) navigate(fallback, true); });
}

import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import test from 'node:test';
import { parseRoute, routePath } from './src/navigation.ts';

test('guide screen and setting references resolve through application navigation', () => {
  const directory = new URL('./guide/', import.meta.url);
  const screens: Record<string, { title: string; path: string }> = JSON.parse(
    readFileSync(new URL('./src/guide-screens.json', import.meta.url), 'utf8'),
  );
  const pages = readdirSync(directory).filter(file => file.endsWith('.md'));
  const references = pages.flatMap(file => [...readFileSync(new URL(file, directory), 'utf8').matchAll(/\[\[([^\]]+)\]\]/g)]);
  assert(references.length > 0);
  for (const [, id] of references) {
    assert(Object.hasOwn(screens, id), `Unknown guide screen ${id}`);
    const screen = screens[id];
    const route = parseRoute(screen.path);
    assert(!route.notFound && (route.settings || route.view), `Unknown app route for ${id}`);
    assert.equal(routePath(route), screen.path);
    assert.equal(id, route.settings ? `settings.${route.settings}` : route.view);
    assert(screen.title.trim(), `Missing label for ${id}`);
  }
});

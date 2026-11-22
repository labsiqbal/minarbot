import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';

type Matcher = { kind: 'exact' | 'prefix' | 'regex' | 'regex-exact'; pattern: string; flags?: string; endpoint?: boolean };
type Inventory = { matchers: Matcher[]; publicMatchers: { pattern: string; flags?: string; endpoint?: boolean }[] };
const inventory = JSON.parse(readFileSync(new URL('./node-route-inventory.json', import.meta.url), 'utf8')) as Inventory;
const matcher = (entry: Matcher) => {
  if (entry.kind === 'exact') return (path: string) => path === entry.pattern;
  if (entry.kind === 'prefix') return (path: string) => path.startsWith(entry.pattern);
  const expression = new RegExp(entry.pattern, entry.flags);
  return (path: string) => expression.test(path);
};

test('Rust route inventory stays within frozen Node matchers and reports ported coverage', t => {
  const rustOnly = new Map([
    ['/api/restore/reconcile', 'Exchanges a backup bootstrap token for a read-only recovery session.'],
    ['/api/agents/{id}/folder', 'Reads or updates an agent working folder.'],
    ['/api/agents/{id}/folder/reset', 'Restores the managed agent working folder.'],
    ['/api/agents/{id}/history/search', 'Searches an agent’s indexed history across conversations.'],
    ['/api/channels/{id}/history/search', 'Searches a channel’s indexed history across conversations.'],
    ['/api/sidebar/stream', 'Streams pending input and turn completion to the local owner.'],
    ['/api/automation-calendar', 'Reads automation occurrences in a bounded local calendar range.'],
    ['/api/automation-calendar/stream', 'Streams local calendar invalidations.'],
    ['/api/automation-calendar/routines.ics', 'Downloads one local project’s routine schedules.'],
    ['/api/routines/{id}', 'Reads a single routine for revision-checked scheduling.'],
    ['/api/provider-accounts/test', 'Tests a new local provider account.'],
    ['/api/provider-accounts/{id}/test', 'Tests a saved local provider account.'],
    ['/api/provider-accounts/{id}/edit', 'Updates a saved local provider account.'],
    ['/api/provider-accounts/{id}/delete', 'Removes a saved local provider account.'],
    ['/api/settings/library-docs', 'Checks Context7 reachability and Vault key status.'],
    ['/api/settings/web-search', 'Reads or saves the local SearXNG URL.'],
    ['/api/settings/web-search/test', 'Tests a SearXNG URL with one JSON search.'],
    ['/api/settings/craft-anything', 'Reads local Craft Anything installation and grants.'],
    ['/api/settings/craft-anything/{action}', 'Manages the local Craft Anything install and grants.'],
    ['/api/engine-logins', 'Lists and creates local engine login profiles.'],
    ['/api/engine-logins/{id}/check', 'Checks sign-in for a local engine login.'],
    ['/api/engine-logins/{id}/remove', 'Removes an unused local engine login.'],
    ['/api/turns/{id}/inspect', 'Reads redacted local run diagnostics for the owner.'],
    ['/api/guide/changes/{id}', 'Applies or cancels a proposed change from the built-in Guide.'],
    ['/api/settings/computer', 'Reads device readiness and starts local Computer setup.'],
    ['/api/connected-apps/discord', 'Reads connection state and routes for the local Discord bot.'],
    ['/api/connected-apps/discord/{action}', 'Connects the bot or changes Discord routes from local Settings.'],
    ['/api/whatsapp', 'Reads the local WhatsApp connection state.'],
    ['/api/whatsapp/{action}', 'Links, unlinks or maps the local WhatsApp connection.'],
    ['/api/email/settings', 'Configures the agent receiving domain and issues the relay secret.'],
    ['/api/email/inbound', 'Receives signed raw email from the relay.'],
    ['/api/agents/{id}/email', 'Lists the selected agent inbox.'],
    ['/api/agents/{id}/email/messages/{message}', 'Reads a message in the selected agent inbox.'],
    ['/api/{kind}/{id}/html-replies/{reply}', 'Reads a rendered HTML reply.'],
    ['/api/vault', 'Lists Vault entry metadata or saves an entry with agent grants.'],
    ['/api/vault/delete', 'Deletes a Vault entry at the expected revision.'],
    ['/api/vault/respond', 'Resolves a Computer secure-fill request.'],
  ]);
  const matchers = new Map(inventory.matchers.map(entry => [entry.kind.startsWith('regex') ? `/${entry.pattern}/${entry.flags ?? ''}` : entry.pattern, matcher(entry)]));
  const endpoints = new Map(inventory.matchers.filter(entry => entry.endpoint === true).map(entry => [entry.kind.startsWith('regex') ? `/${entry.pattern}/${entry.flags ?? ''}` : entry.pattern, matcher(entry)]));
  const publicMatchers = inventory.publicMatchers.map(entry => new RegExp(entry.pattern, entry.flags));
  const src = new URL('../crates/minarbot-server/src/', import.meta.url);
  const rust = readdirSync(src, { recursive: true }).map(String)
    .filter(name => name.endsWith('.rs') && !/(^|\/)tests(\/|\.rs$)|_tests\.rs$/.test(name))
    .map(name => readFileSync(new URL(name, src), 'utf8')).join('\n');
  const routes = [...rust.matchAll(/\.route\(\s*"([^"\n]+)"/g)].map(match => match[1]);
  assert(routes.length > 0);
  const duplicates = routes.filter((route, index) => routes.indexOf(route) !== index);
  assert.deepEqual(duplicates, [], `Rust routes registered twice: ${duplicates.join(', ')}`);
  const id = '00000000-0000-4000-8000-000000000001';
  const ported = new Set<string>();
  for (const route of routes) {
    const witness = route.replace(/\{seq\}/g, '1').replace(/\{[^}]+\}/g, id);
    if (!/^\/(?:api(?:\/|$)|mcp(?:\/|$))/.test(route)) {
      assert(publicMatchers.some(publicMatcher => publicMatcher.test(witness)), `Rust public route absent from Node: ${route}`);
      continue;
    }
    const environment = witness.replace(/^\/api\/projects\/[^/]+(\/environment(?:\/(?:remove|settings))?)$/, '/api$1');
    const matches = [...endpoints].filter(([, testMatcher]) => testMatcher(witness) || testMatcher(environment));
    assert(matches.length > 0 || rustOnly.delete(route), `Rust route absent from Node: ${route}`);
    for (const [name] of matches) ported.add(name);
  }
  assert.deepEqual([...rustOnly.keys()], [], 'Unused Rust-only route entries');
  const witnesses = routes.map(route => route.replace(/\{seq\}/g, '1').replace(/\{[^}]+\}/g, id));
  for (const [name, testMatcher] of matchers) if (!endpoints.has(name) && witnesses.some(witness => testMatcher(witness))) ported.add(name);
  assert(![...endpoints.values()].some(testMatcher => testMatcher('/api/__inventory_unknown')));
  assert(![...endpoints.values()].some(testMatcher => testMatcher(`/api/agents/${id}/__inventory_unknown`)));
  assert.equal(matchers.size, 206);
  assert.equal(ported.size, 206);
  t.diagnostic(`ported ${ported.size}/${matchers.size} (${routes.length} Rust route registrations)`);
  for (const name of matchers.keys()) if (!ported.has(name)) t.diagnostic(`missing ${name}`);
  for (const route of routes) t.diagnostic(route);
});

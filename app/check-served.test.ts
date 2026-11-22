import { test, type TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { xtask } from './test-support/release.ts';

const run = promisify(execFile);
async function checkServed(origin: string, options: { present?: string[]; absent?: string[]; expectVersion?: string } = {}) {
  const args = ['check-served', origin, ...Object.entries(options).flatMap(([key, values]) =>
    [values].flat().flatMap(value => [key === 'expectVersion' ? '--expect-version' : '--' + key, value]))];
  try {
    const { stdout } = await run(xtask(), args);
    return { ok: true, report: stdout.trim().split('\n') };
  } catch (error) {
    const { stdout } = error as { stdout: string };
    if (!stdout.includes('Served check FAILED.')) throw error;
    return { ok: false, report: stdout.trim().split('\n') };
  }
}

async function fixture(t: TestContext) {
  const requests: string[] = [];
  const routes = new Map([
    ['/', ['text/html', '<script type="module" src="/assets/index-123.js"></script><link rel="stylesheet" href="/assets/index-123.css">']],
    ['/assets/index-123.js', ['text/javascript', 'const marker = "feature shipped";']],
    ['/assets/index-123.css', ['text/css', '.shipped-style { color: red; }']],
    ['/api/updates', ['application/json', JSON.stringify({ current: { version: '1.2.3' }, available: { version: '9.9.9' } })]],
  ]);
  let origin = '';
  const server = createServer((req, res) => {
    requests.push(req.url!);
    if (req.method !== 'GET' || req.headers.origin !== origin || req.headers['sec-fetch-site'] !== 'same-origin') {
      res.writeHead(403).end('Wrong request method or headers'); return;
    }
    const route = routes.get(req.url!);
    if (!route) { res.writeHead(404).end('Not found'); return; }
    if (route[0] === 'redirect') { res.writeHead(302, { Location: route[1] }).end(); return; }
    res.writeHead(200, { 'Content-Type': route[0] }).end(route[1]);
  });
  await new Promise<void>(done => server.listen(0, '127.0.0.1', done));
  const address = server.address(); assert(address && typeof address !== 'string');
  origin = `http://127.0.0.1:${address.port}`;
  t.after(() => new Promise<void>((done, reject) => server.close(error => error ? reject(error) : done())));
  return { origin, routes, requests };
}

test('served check fetches index, JS, CSS and local version with GET owner headers', async t => {
  const f = await fixture(t);
  const report = await checkServed(f.origin, { present: ['feature shipped', 'shipped-style'], absent: ['old feature'], expectVersion: '1.2.3' });
  assert.equal(report.ok, true);
  assert.deepEqual(f.requests, ['/', '/assets/index-123.js', '/assets/index-123.css', '/api/updates']);
});

test('served check reports every marker and version mismatch across HTML, JS and CSS', async t => {
  const f = await fixture(t);
  const result = await checkServed(f.origin, { present: ['missing one', 'missing two'], absent: ['feature shipped', 'shipped-style', 'type="module"'], expectVersion: '9.9.9' });
  assert.equal(result.ok, false);
  assert.equal(result.report.filter((line: string) => line.startsWith('FAIL')).length, 6);
  assert.match(result.report.join('\n'), /expected 9\.9\.9, served 1\.2\.3/);
  assert.match(result.report.join('\n'), /absent "shipped-style" in \/assets\/index-123.css/);
});

test('served check accepts relative, absolute and preload refs with mixed attribute order and quotes', async t => {
  const f = await fixture(t);
  f.routes.set('/', ['text/html', `<!-- <script src="/missing.js"></script> --><LINK href='${f.origin}/assets/index-123.css?v=1&amp;x=2' rel='stylesheet'><script defer src=assets/index-123.js></script><link href="/assets/preload.js" rel="modulepreload">`]);
  f.routes.set('/assets/index-123.css?v=1&x=2', ['text/css', 'versioned css']);
  f.routes.set('/assets/preload.js', ['application/javascript', 'preload feature']);
  assert.equal((await checkServed(f.origin, { present: ['versioned css', 'preload feature'] })).ok, true);
  assert.equal(f.requests.filter(path => path === '/api/updates').length, 0);
});

for (const kind of ['http', 'content-type', 'no-assets', 'external', 'bad-version-json', 'missing-version'] as const) test(`served check fails on ${kind}`, async t => {
  const f = await fixture(t);
  if (kind === 'http') f.routes.delete('/assets/index-123.css');
  if (kind === 'content-type') f.routes.set('/assets/index-123.js', ['text/html', '<title>SPA fallback</title>']);
  if (kind === 'no-assets') f.routes.set('/', ['text/html', '<title>No built assets</title>']);
  if (kind === 'external') f.routes.set('/', ['text/html', '<script src="https://example.invalid/assets/index.js"></script><link href="/assets/index-123.css" rel="stylesheet">']);
  if (kind === 'bad-version-json') f.routes.set('/api/updates', ['application/json', 'not json']);
  if (kind === 'missing-version') f.routes.set('/api/updates', ['application/json', '{"machines":[]}']);
  if (kind === 'missing-version') {
    const result = await checkServed(f.origin, { expectVersion: '1.2.3' });
    assert.equal(result.ok, false);
    assert.match(result.report.join('\n'), /missing current.version/);
  } else await assert.rejects(checkServed(f.origin, { expectVersion: '1.2.3' }), {
    http: /index-123.css.*HTTP 404/,
    'content-type': /index-123.js.*Content-Type/,
    'no-assets': /no JS assets/,
    external: /outside origin/,
    'bad-version-json': /JSON/,
  }[kind]);
});

test('served CLI repeats markers and exits nonzero on mismatch or invalid input', async t => {
  const f = await fixture(t);
  const { stdout } = await run(xtask(), ['check-served', f.origin, '--present', 'feature shipped', '--present', 'shipped-style', '--absent', 'removed', '--absent', 'retired', '--expect-version', '1.2.3']);
  assert.match(stdout, /Served check passed/);
  await assert.rejects(run(xtask(), ['check-served', f.origin, '--present', 'missing', '--absent', 'shipped-style', '--expect-version', '9.9.9']), (error: any) => {
    assert.equal(error.code, 1);
    assert.match(error.stdout, /FAIL present "missing"/);
    assert.match(error.stdout, /FAIL absent "shipped-style"/);
    assert.match(error.stdout, /FAIL version/);
    return true;
  });
  for (const args of [[], [f.origin, '--present'], [f.origin, '--present', ''], [f.origin + '/path'], ['file:///tmp/index.html'], [f.origin, '--unknown']]) await assert.rejects(run(xtask(), ['check-served', ...args]), (error: any) => error.code === 1 && /Served check FAILED/.test(error.stderr));
});


test('served check rejects redirects without requesting their target', async t => {
  const f = await fixture(t);
  f.routes.set('/assets/index-123.js', ['redirect', '/login']);
  await assert.rejects(checkServed(f.origin), /GET .*index-123.js failed/);
  assert.deepEqual(f.requests, ['/', '/assets/index-123.js']);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync, readdirSync, readFileSync } from 'node:fs';
import { createServer, type RequestListener } from 'node:http';
import { createServer as createTlsServer } from 'node:https';
import { execFile, execFileSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { promisify } from 'node:util';
import sharp from 'sharp';
import { startApp } from './test-bridge.ts';
import { connectVnc } from './test-support/agent-desktop-test-client.ts';

if (process.env.MINARBOT_DESKTOP_IT !== '1') throw new Error('Opt-in check: MINARBOT_DESKTOP_IT=1 node --experimental-strip-types --test rust-desktop.integration.test.ts');

async function until(condition: () => boolean | Promise<boolean>) {
  const deadline = performance.now() + 15_000;
  while (performance.now() < deadline) {
    if (await condition()) return;
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  assert.fail('Desktop interaction not observed by fixture.');
}

function processInfo(pid: string) {
  try {
    const fields = readFileSync(`/proc/${pid}/stat`, 'utf8').split(') ').at(-1)!.split(' ');
    const args = readFileSync(`/proc/${pid}/cmdline`, 'utf8').split('\0').filter(Boolean);
    return { pid, parent: fields[1], birth: fields[19], args, live: fields[0] !== 'Z' };
  } catch { return undefined; }
}

function desktopProcesses(profile: string) {
  const processes = readdirSync('/proc').filter(pid => /^\d+$/.test(pid)).map(processInfo).filter(row => row?.live);
  const owned = new Set(processes.filter(row => row?.args.includes(profile)).map(row => row!.pid));
  for (let pass = 0; pass < 10; pass++) for (const row of processes) if (row && owned.has(row.parent)) owned.add(row.pid);
  return processes.filter(row => row && owned.has(row.pid)).map(row => row!);
}

test('Linux sandbox: public fixture, input, file isolation, loopback denial, SIGKILL and stale recovery', { timeout: 180_000 }, async () => {
  const dir = mkdtempSync(join(tmpdir(), 'bs-desktop-it-'));
  const agent = randomUUID(), path = `/api/agents/${agent}/computer`, profile = join(dir, 'agent-desktops', agent, 'chrome');
  const marker = join(dir, 'host-private-marker.txt'), proof = join(profile, '..', 'home', 'network.json');
  writeFileSync(marker, 'HOST_PRIVATE');
  let loaded = false, clicked = false, focused = false, blockedHits = 0;
  const typed = new Set<string>();
  const handler: RequestListener = (request, response) => {
    const url = new URL(request.url!, 'http://fixture.test');
    if (url.pathname === '/loaded') loaded = true;
    if (url.pathname === '/clicked') clicked = true;
    if (url.pathname === '/focused') focused = true;
    if (url.pathname === '/typed') typed.add(url.searchParams.get('value') ?? '');
    if (url.pathname === '/private') blockedHits++;
    response.setHeader('Content-Type', 'text/html');
    response.end(`<!doctype html><title>Desktop fixture</title>
      <button style="position:fixed;left:100px;top:100px;width:300px;height:120px" onclick="fetch('/clicked');this.textContent='Click received'">Click desktop</button>
      <input style="position:fixed;left:100px;top:300px;width:500px;height:60px" aria-label="Test input" onfocus="fetch('/focused')" oninput="fetch('/typed?value='+encodeURIComponent(this.value))">
      <script>fetch('/loaded')</script>`);
  };
  execFileSync('/usr/bin/openssl', ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', join(dir, 'key.pem'), '-out', join(dir, 'cert.pem'), '-days', '1', '-subj', '/CN=public-fixture.example.test'], { stdio: 'ignore' });
  const http = createServer(handler);
  const tls = createTlsServer({ key: readFileSync(join(dir, 'key.pem')), cert: readFileSync(join(dir, 'cert.pem')) }, handler);
  async function listen(server: typeof http | typeof tls) {
    await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
    const address = server.address(); assert(address && typeof address !== 'string'); return address.port;
  }
  const port = await listen(http), httpsPort = await listen(tls);
  const options = { database: join(dir, 'fixture.sqlite'), desktop: { dataDir: dir, enabled: true, idleMs: 60_000, leaseDir: join(dir, 'leases'), testOnlyProxyTarget: { hostname: 'public-fixture.example.test', port, httpsPort } } };
  let app: Awaited<ReturnType<typeof startApp>> | undefined;
  const post = async (route: string, body: unknown, status?: number) => {
    assert(app);
    const response = await fetch(app.origin + route, { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
    const value = await response.json();
    if (status) assert.equal(response.status, status, JSON.stringify(value));
    else assert(response.ok, JSON.stringify(value));
    return value;
  };
  try {
    app = await startApp(options);
    await post('/api/agents', { runtime: 'api', id: agent, name: 'Desktop fixture', model: 'fixture', instructions: 'Help.' });
    await post(path, { id: randomUUID(), revision: 0, enabled: true });
    await post(path + '/start', {});
    const launch = desktopProcesses(profile).find(row => row.args[0] === '/usr/bin/bwrap' && row.args.includes('--unshare-net'));
    assert(launch, 'Rust manager must launch Bubblewrap with the private profile and network namespace');
    const command = launch.args.indexOf('--') + 1;
    assert.deepEqual(launch.args.slice(command), ['/minarbot-server', '--desktop-supervisor', '/run/minarbot/proxy.sock']);
    assert(desktopProcesses(profile).some(row => row.args[0] === '/minarbot-server' && row.args[1] === '--desktop-supervisor'));
    const hiddenPaths = [marker, process.env.HOME!, dir, resolve('..'), `/run/user/${process.getuid!()}`];
    const inspect = 'for path do if test -e "$path"; then printf "Host path visible: %s\\n" "$path" >&2; exit 1; fi; done; printf "Host paths absent\\n"';
    const sandbox = await promisify(execFile)('/usr/bin/bwrap', [...launch.args.slice(1, command), '/usr/bin/sh', '-c', inspect, 'check-mounts', ...hiddenPaths], { env: { PATH: '/usr/bin:/bin', HOME: '/home/desktop' }, timeout: 5000 });
    assert.match(sandbox.stdout, /Host paths absent/);
    const take = await post(path + '/control', { id: randomUUID(), revision: 0, operation: 'take' });
    const input = (name: string, args: unknown) => post(path + '/input', { name, args, revision: take.control.revision });
    const key = (...keys: string[]) => input('computer_key', { keys });
    const type = (text: string) => input('computer_type', { text });
    await input('computer_click', { x: 16, y: 16 });
    await new Promise(resolve => setTimeout(resolve, 700));
    const direct = "env -u http_proxy -u https_proxy -u HTTP_PROXY -u HTTPS_PROXY -u ALL_PROXY -u all_proxy curl --noproxy '*' --connect-timeout 2 --max-time 3 -fsS";
    await type(`${direct} http://127.0.0.1:${port}/private >/tmp/local 2>&1; local=$?; ${direct} https://example.com >/tmp/direct 2>&1; direct=$?; ${direct} --resolve example.com:443:93.184.216.34 https://example.com >/tmp/resolved 2>&1; resolved=$?; curl -kfsS --max-time 5 https://public-fixture.example.test/ >/tmp/proxy; proxied=$?; printf '{"local":%s,"direct":%s,"resolved":%s,"proxied":%s}\\n' "$local" "$direct" "$resolved" "$proxied" > "$HOME/network.json"`);
    await key('Return');
    await until(() => { try { return Boolean(JSON.parse(readFileSync(proof, 'utf8'))); } catch { return false; } });
    const network = JSON.parse(readFileSync(proof, 'utf8'));
    for (const field of ['local', 'direct', 'resolved']) assert.notEqual(network[field], 0);
    assert.equal(network.proxied, 0);
    assert.equal(blockedHits, 0);
    loaded = false;
    await input('computer_open_url', { url: `http://public-fixture.example.test:${port}/` }); await until(() => loaded);
    await key('F11'); await new Promise(resolve => setTimeout(resolve, 500));
    await input('computer_click', { x: 250, y: 160 }); await until(() => clicked);
    await input('computer_click', { x: 250, y: 330 }); await until(() => focused);
    await type('Minarbot desktop works'); await until(() => typed.has('Minarbot desktop works'));
    const screen = await fetch(app.origin + path + '/screenshot');
    assert(screen.ok);
    const image = await sharp(Buffer.from(await screen.arrayBuffer())).metadata();
    assert.deepEqual([image.format, image.width, image.height], ['jpeg', 960, 600]);

    const ticket = await post(path + '/vnc-ticket', { mode: 'control', revision: take.control.revision });
    const viewer = await connectVnc(app.origin + path + '/vnc?ticket=' + ticket.ticket, app.origin);
    try {
      await viewer.handshake(); assert.deepEqual([viewer.width, viewer.height], [1280, 800]);
      viewer.requestFrame(false); const before = await viewer.frame();
      clicked = false; focused = false;
      viewer.pointer(250, 160, 1); viewer.pointer(250, 160, 0); await until(() => clicked);
      viewer.pointer(250, 330, 1); viewer.pointer(250, 330, 0); await until(() => focused);
      viewer.key(0xff57, true); viewer.key(0xff57, false);
      viewer.text(' via VNC'); await until(() => typed.has('Minarbot desktop works via VNC'));
      viewer.requestFrame(); assert.notDeepEqual(await viewer.frame(), before);
    } finally { viewer.close(); }

    await key('F11');
    await key('Control_L', 'l'); await type(`http://127.0.0.1:${port}/private`); await key('Return');
    await new Promise(resolve => setTimeout(resolve, 400));
    assert.equal(blockedHits, 0);
    const original = desktopProcesses(profile);
    // Chrome rewrites /proc cmdline to a single space-separated process title.
    const chrome = original.map(row => row.args.join(' ')).find(args => args.includes('--user-data-dir=/home/desktop/chrome') && !args.includes('--type='));
    assert(chrome);
    if (!chrome.includes('--no-sandbox')) assert(original.some(row => {
      try {
        return row.args.join(' ').includes('--type=renderer') && /^Seccomp:\s+2$/m.test(readFileSync(`/proc/${row.pid}/status`, 'utf8'));
      } catch { return false; }
    }));
    const returned = await post(path + '/control', { id: randomUUID(), revision: take.control.revision, operation: 'hand-back' });
    assert.equal(returned.control.mode, 'agent');
    await post(path + '/input', { revision: take.control.revision, name: 'computer_click', args: { x: 1, y: 1 } }, 409);
    await app.crash();
    await until(() => original.every(row => { const now = processInfo(row.pid); return !now?.live || now.birth !== row.birth; }) && !desktopProcesses(profile).length);
    app = await startApp(options);
    await post(path + '/start', {});
    assert((await fetch(app.origin + path + '/screenshot')).ok);
    assert.equal((await post(path + '/stop', {})).state, 'off');
    await until(() => !desktopProcesses(profile).length);
    console.log(JSON.stringify({ network, clicked, typed: typed.has('Minarbot desktop works via VNC'), filesystemHidden: true, crashCleanup: true, staleLeaseRecovery: true }));
  } finally {
    await app?.close();
    http.closeAllConnections(); tls.closeAllConnections();
    await Promise.all([http, tls].map(server => new Promise<void>(resolve => server.close(() => resolve()))));
    rmSync(dir, { recursive: true, force: true });
  }
});

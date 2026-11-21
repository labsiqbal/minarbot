import assert from 'node:assert/strict';
import { execFile, execFileSync } from 'node:child_process';
import { createHash, generateKeyPairSync, randomUUID, sign } from 'node:crypto';
import { copyFileSync, cpSync, createReadStream, existsSync, mkdirSync, mkdtempSync, readFileSync, readlinkSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { basename, join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { fileManifest } from './release-lib.mjs';
import { freePort } from './test-port.mjs';

const root = fileURLToPath(new URL('..', import.meta.url));
const binaryDir = resolve(process.argv[2] ?? 'target/debug');
const mac = process.platform === 'darwin';
const scratch = mkdtempSync(join(tmpdir(), 'minarbot-install-'));
const key = generateKeyPairSync('ed25519');
const installer = join(scratch, 'install.sh');
const run = promisify(execFile);
const homes = [];
const { version } = JSON.parse(readFileSync(join(root, 'app/package.json')));
const commit = '1234567890abcdef1234567890abcdef12345678', id = version + '-' + commit.slice(0, 8);
const archive = join(scratch, `minarbot-${version}-${mac ? 'macos' : 'linux'}-${process.arch === 'arm64' ? 'aarch64' : 'x86_64'}.tar.gz`);
let manifest, signature, mode = 'valid';
const fixture = createServer((req, res) => {
  const send = data => res.writeHead(200, { 'Content-Length': Buffer.byteLength(data) }).end(data);
  if (req.url === '/latest/download/latest.json') return send(manifest);
  if (req.url === '/latest/download/latest.json.sig') return send(mode === 'signature' ? Buffer.alloc(64).toString('base64') : signature);
  if (req.url === `/download/v${version}/${basename(archive)}`) {
    if (mode === 'digest') return send(Buffer.alloc(statSync(archive).size));
    res.writeHead(200, { 'Content-Length': statSync(archive).size });
    return createReadStream(archive).pipe(res);
  }
  if (req.url === '/v1/chat/completions') {
    req.resume();
    return res.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: 'Installed and ready.' } }] }));
  }
  res.writeHead(404).end();
});
const domain = mac && execFileSync('id', ['-u'], { encoding: 'utf8' }).trim();
let launchDomain;

async function request(origin, path, value) {
  const response = await fetch(origin + path, value === undefined ? {} : {
    method: 'POST', headers: { Origin: origin, 'Content-Type': 'application/json' },
    body: JSON.stringify(value),
  });
  assert(response.ok, `${path}: HTTP ${response.status} ${await response.clone().text()}`);
  return response.json();
}

try {
  const content = join(scratch, 'content');
  mkdirSync(join(content, 'bin'), { recursive: true });
  for (const binary of ['minarbot-server', 'minarbot-launcher']) {
    copyFileSync(join(binaryDir, binary), join(content, 'bin', binary));
  }
  copyFileSync(join(root, 'app/package.json'), join(content, 'package.json'));
  cpSync(join(root, 'app/dist'), join(content, 'dist'), { recursive: true });
  writeFileSync(join(content, 'release.json'), JSON.stringify({ id, version, commit, files: fileManifest(content) }));
  execFileSync('tar', ['--format', 'ustar', '-czf', archive, '-C', content, '.'], { env: { ...process.env, COPYFILE_DISABLE: '1' } });
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(archive)) hash.update(chunk);
  manifest = JSON.stringify({ id, version, commit, assets: { [basename(archive)]: { sha256: hash.digest('hex'), size: statSync(archive).size } } });
  signature = sign(null, Buffer.from(manifest), key.privateKey).toString('base64');
  const publicKey = Buffer.from(key.publicKey.export({ format: 'jwk' }).x, 'base64url').toString('base64');
  writeFileSync(installer, readFileSync(join(root, 'scripts/install.sh'), 'utf8').replace(
    /^PUBLIC_KEY = '.*'$/m, `PUBLIC_KEY = '${publicKey}'`));
  await new Promise(done => fixture.listen(0, '127.0.0.1', done));
  const base = `http://127.0.0.1:${fixture.address().port}`;

  if (mac) {
    launchDomain = 'gui/' + domain;
    if ((await run('launchctl', ['print', launchDomain]).catch(error => error)).code) launchDomain = 'user/' + domain;
    const existing = await run('launchctl', ['print', launchDomain + '/com.minarbot.server']).catch(error => error);
    assert(existing.code, 'Smoke test requires an unused Minarbot launchd label.');
  }
  const bin = join(scratch, 'path');
  mkdirSync(bin);
  writeFileSync(join(bin, mac ? 'open' : 'xdg-open'), '#!/bin/sh\nprintf "%s\\n" "$1" > "$HOME/opened-url"\n', { mode: 0o700 });
  writeFileSync(join(bin, 'systemctl'), `#!/bin/sh
set -eu
[ "$*" = '--user daemon-reload' ] && exit 0
[ "$*" = '--user enable --now minarbot.service' ]
unit=$HOME/.config/systemd/user/minarbot.service
test -s "$unit"
[ ! -f "$HOME/launcher.pid" ] || exit 0
nohup "$HOME/.local/share/minarbot/releases/current/bin/minarbot-launcher" --config "$HOME/.config/minarbot/launcher.json" >"$HOME/server.log" 2>&1 </dev/null &
echo "$!" > "$HOME/launcher.pid"
`, { mode: 0o700 });
  const home = join(scratch, 'home');
  homes.push(home);
  const cfg = join(home, '.config/minarbot');
  mkdirSync(cfg, { recursive: true });
  const envFile = join(cfg, 'launcher.env');
  const config = join(cfg, 'launcher.json');
  const port = await freePort();
  const args = [installer, '--port', String(port)];
  const env = { PATH: bin + ':' + process.env.PATH, HOME: home, MINARBOT_RELEASE_BASE: base };
  for (const [failure, text] of [['signature', /Invalid GitHub release signature/], ['digest', /SHA-256 mismatch/]]) {
    mode = failure;
    await assert.rejects(run('sh', args, { env }), text);
    assert(!existsSync(config));
    assert(!existsSync(join(home, '.local/share/minarbot/releases/current')));
  }
  mode = 'valid';
  const provider = `OPENAI_BASE_URL=${base}/v1\nOPENAI_MODEL=fixture\n`;
  writeFileSync(envFile, provider, { mode: 0o600 });
  const origin = `http://127.0.0.1:${port}`;
  const { stdout } = await run('sh', args, { env });
  assert.match(stdout, /is ready at/);
  const opened = join(home, 'opened-url');
  for (let n = 0; n < 30 && !existsSync(opened); n++) await delay(100);
  assert.equal(readFileSync(opened, 'utf8').trim(), origin);
  assert.equal(readlinkSync(join(home, '.local/share/minarbot/releases/current')), id);
  assert.equal(statSync(config).mode & 0o777, 0o600);
  assert.equal(readFileSync(envFile, 'utf8'), provider);
  if (!mac) assert.equal(readFileSync(join(home, '.config/systemd/user/minarbot.service'), 'utf8'),
    readFileSync(join(root, 'scripts/minarbot.service'), 'utf8'));
  assert.equal((await request(origin, '/api/environment')).version, version);
  const index = await fetch(origin);
  assert(index.ok);
  const html = await index.text();
  assert.match(html, /<title>Minarbot<\/title>/);
  const bundle = /<script[^>]+src="([^"\s]+)"/.exec(html)?.[1];
  assert(bundle);
  assert((await fetch(new URL(bundle, origin))).ok);
  const agent = randomUUID(), turn = randomUUID(), turns = `/api/agents/${agent}/turns`;
  await request(origin, '/api/agents', { id: agent, name: 'Smoke', instructions: 'Answer.', model: 'fixture', runtime: 'api' });
  if (mac) {
    const computer = await request(origin, `/api/agents/${agent}/computer`);
    assert.equal(computer.available, false);
    assert.equal(computer.reason, 'Computer is unavailable on this platform.');
  }
  await request(origin, turns, { id: turn, text: 'Reply.' });
  let result;
  for (let n = 0; n < 100; n++) {
    result = (await request(origin, turns)).find(row => row.id === turn);
    if (['done', 'failed'].includes(result?.status)) break;
    await delay(100);
  }
  assert.equal(result?.status, 'done', result?.error);
  assert.equal(result.reply, 'Installed and ready.');
  await run('sh', args, { env });
  if (!mac) {
    const { token } = await request(origin, '/api/install/tokens', { id: randomUUID() });
    const peer = join(scratch, 'peer');
    homes.push(peer);
    mkdirSync(peer);
    const peerPort = await freePort();
    await run('sh', [installer, '--no-open', '--port', String(peerPort), '--source', origin, '--token', token], { env: { ...env, HOME: peer } }).catch(error => {
      throw new Error(error.stderr || 'Peer installer failed.');
    });
    const machines = await request(`http://127.0.0.1:${peerPort}`, '/api/machines');
    assert(machines.machines.some(machine => machine.origin === origin && machine.pairingState === 'paired'));
    assert((await request(origin, '/api/machines')).machines.some(machine =>
      machine.origin === `http://127.0.0.1:${peerPort}` && machine.pairingState === 'paired'));
  }
  console.log('Installer smoke passed: signature, digest, private config, service, UI, fake provider turn, rerun' + (mac ? '.' : ', pairing.'));
} catch (error) {
  for (const home of homes) {
    for (const log of [join(home, 'server.log'), join(home, '.local/share/minarbot/server.log')]) {
      if (existsSync(log)) console.error(readFileSync(log, 'utf8'));
    }
  }
  throw error;
} finally {
  if (launchDomain && homes.length) await run('launchctl', ['bootout', launchDomain + '/com.minarbot.server']).catch(() => {});
  for (const home of homes) {
    const file = join(home, 'launcher.pid');
    if (existsSync(file)) {
      const pid = Number(readFileSync(file));
      try {
        const args = readFileSync(`/proc/${pid}/cmdline`, 'utf8').split('\0');
        if (args[0] !== join(home, '.local/share/minarbot/releases/current/bin/minarbot-launcher')) continue;
        process.kill(pid, 'SIGTERM');
        for (let n = 0; n < 30; n++) { process.kill(pid, 0); await delay(100); }
      } catch (error) {
        if (!['ESRCH', 'ENOENT'].includes(error.code)) throw error;
      }
    }
  }
  fixture.closeAllConnections();
  fixture.close();
  rmSync(scratch, { recursive: true, force: true });
}

import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import { buildEnvironmentCaptureCommand, hydratePosixHome, hydratePosixPath, listLoginShellCandidates, mergePathEntries, readEnvironmentFromLoginShell } from './t3/path.ts';
import { rustBinary } from './rust-binary.ts';
import { freePort, stop } from './test-support/test-process.ts';

for (const broken of [false, true]) test(broken ? 'broken login shell keeps service startup working' : 'service launch discovers Codex exported only by the login shell', { timeout: 20_000 }, async t => {
  const directory = mkdtempSync(join(tmpdir(), 'minarbot-login-path-'));
  const bin = join(directory, 'bin'), shell = join(directory, 'login-shell');
  const inherited = join(directory, 'service-bin');
  const probes = join(directory, 'probes'), environment = join(directory, 'harness-env.json'), statusEnvironment = join(directory, 'status-env.json');
  const binary = rustBinary('minarbot-server', process.env.MINARBOT_RUST_BINARY);
  mkdirSync(bin);
  mkdirSync(inherited);
  writeFileSync(join(inherited, 'printenv'), '#!/bin/sh\n[ "$1" = PATH ] && printf "%s\\n" "$PATH"\n', { mode: 0o700 });
  // The fallback Bash login shell also has a fixture-only PATH.
  writeFileSync(join(directory, '.bash_profile'), `export PATH='${inherited}'\n`);
  writeFileSync(join(bin, 'codex'), `#!${process.execPath}
import { writeFileSync } from 'node:fs';
const status = process.argv[2] === 'login' && process.argv[3] === 'status';
writeFileSync(status ? ${JSON.stringify(statusEnvironment)} : ${JSON.stringify(environment)}, JSON.stringify(process.env));
if (status) process.exit(1);
console.log('codex fixture 1.0');
`, { mode: 0o700 });
  writeFileSync(shell, broken
    ? '#!/bin/sh\necho synthetic-secret >&2\nexit 7\n'
    : `#!/bin/sh
printf x >> '${probes}'
printf 'profile noise\\n'
export PATH='${bin}:${inherited}'
export SHELL_SECRET=synthetic-secret
exec /bin/sh -c "$2"
`, { mode: 0o700 });
  const port = await freePort();
  const origin = `http://127.0.0.1:${port}`;
  const child = spawn(binary, [], {
    env: { HOME: directory, PATH: inherited, SHELL: shell, TMPDIR: directory, PORT: String(port), MINARBOT_DATA_DIR: directory, AMBIENT_SECRET: 'synthetic-secret', LANG: 'C', OPENAI_API_KEY: 'canary', COMPOSIO_API_KEY: 'canary', NODE_OPTIONS: '--trace-warnings', __MISE_DIFF: 'canary', JAVA_HOME: 'canary' },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  const closed = new Promise(ready => child.once('close', ready));
  let stderr = '';
  child.stderr.on('data', chunk => { stderr += chunk; });
  try {
    const startup = AbortSignal.any([t.signal, AbortSignal.timeout(10_000)]);
    const lines = createInterface({ input: child.stdout, signal: startup });
    let ready = false;
    for await (const line of lines) {
      if (line === `Minarbot: ${origin}`) { ready = true; break; }
    }
    assert(ready, `Server did not report readiness. ${stderr}`);
    const response = await fetch(origin + '/api/environment', { signal: startup });
    assert.equal(response.status, 200, stderr);
    await response.arrayBuffer();
    const catalog = await fetch(origin + '/api/runtimes', { signal: AbortSignal.any([t.signal, AbortSignal.timeout(10_000)]) });
    assert.equal(catalog.status, 200, stderr);
    const runtimes = await catalog.json();
    assert(!stderr.includes('synthetic-secret'));
    const choices: { runtime: string; version: string | null; reason: string | null; signedIn: boolean | null }[] = runtimes.choices;
    const discovered = choices.filter(choice => !choice.reason?.includes('is missing from PATH.'));
    assert.deepEqual(discovered.map(choice => choice.runtime), broken ? [] : ['codex']);
    if (broken) {
      assert.equal(choices.find(choice => choice.runtime === 'codex')?.version, null);
      assert.equal(runtimes.defaultRuntime, 'api');
      return;
    }
    assert.equal(choices.find(choice => choice.runtime === 'codex')?.version, 'codex fixture 1.0');
    assert.equal(runtimes.defaultRuntime, 'codex');
    await (await fetch(origin + '/api/runtimes', { signal: t.signal })).arrayBuffer();
    assert.equal(readFileSync(probes, 'utf8'), 'x');
    const childEnv = JSON.parse(readFileSync(environment, 'utf8'));
    assert.equal(childEnv.PATH, `${bin}:${inherited}`);
    assert.deepEqual(Object.keys(childEnv).sort(), ['HOME', 'LANG', 'PATH', 'TMPDIR']);
    assert(childEnv.HOME.startsWith(join(directory, 'minarbot-probe-')));
    assert.equal(childEnv.LANG, 'C');
    assert.equal(childEnv.TMPDIR, directory);
    // The device sign-in check sees the service's own HOME and nothing else from its environment.
    const statusEnv = JSON.parse(readFileSync(statusEnvironment, 'utf8'));
    assert.deepEqual(Object.keys(statusEnv).sort(), ['CODEX_HOME', 'HOME', 'PATH']);
    assert.equal(statusEnv.HOME, directory); assert.equal(statusEnv.CODEX_HOME, join(directory, '.codex'));
    assert.equal(choices.find(choice => choice.runtime === 'codex')?.signedIn, false);
  } finally {
    await stop(child, closed);
    rmSync(directory, { recursive: true, force: true });
  }
});

test('login shell candidates, PATH ordering, HOME fallback and capture scope', () => {
  assert.deepEqual(listLoginShellCandidates('darwin', ' /opt/bin/fish ', '/bin/zsh'), ['/opt/bin/fish', '/bin/zsh']);
  assert.deepEqual(listLoginShellCandidates('linux', 'bash', '/bin/bash'), ['/bin/bash']);
  assert.equal(mergePathEntries('/custom:/usr/bin:/custom::', '/usr/bin:/bin'), '/custom:/usr/bin:/bin');
  const env = { HOME: ' ' }; hydratePosixHome(env, () => '/account'); assert.equal(env.HOME, '/account');
  hydratePosixHome(env, () => { throw new Error('must preserve HOME'); });
  const capture = buildEnvironmentCaptureCommand('/bin/bash');
  assert(capture.startsWith('(trap - DEBUG)'));
  assert.deepEqual([...capture.matchAll(/printenv ([A-Z_]+)/g)].map(match => match[1]), ['PATH', '__MISE_DIFF', 'JAVA_HOME']);
  assert.match(buildEnvironmentCaptureCommand('/bin/nu'), /ENV_CONVERSIONS/);
});

test('failed and hanging login probes preserve PATH with bounded process-tree cleanup', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'minarbot-broken-shell-'));
  const broken = join(directory, 'broken'), hanging = join(directory, 'hanging');
  const working = join(directory, 'working'), noisy = join(directory, 'noisy');
  writeFileSync(broken, '#!/bin/sh\necho synthetic-secret >&2\nexit 7\n', { mode: 0o700 });
  writeFileSync(hanging, '#!/bin/sh\nsleep 30 &\nwait\n', { mode: 0o700 });
  writeFileSync(working, '#!/bin/sh\nexport PATH=/custom:/usr/bin:/bin JAVA_HOME=/new __MISE_DIFF=fixture\nexec /bin/sh -c "$2"\n', { mode: 0o700 });
  writeFileSync(noisy, '#!/bin/sh\nhead -c 1048577 /dev/zero\n', { mode: 0o700 });
  try {
    const env = { PATH: '/usr/bin::/bin:/usr/bin', JAVA_HOME: '/existing', HOME: directory };
    assert.equal(await hydratePosixPath(env, 'linux', [broken]), false);
    assert.equal(env.PATH, '/usr/bin::/bin:/usr/bin');
    assert.equal(env.JAVA_HOME, '/existing');
    const started = Date.now();
    assert.deepEqual(await readEnvironmentFromLoginShell(hanging, env, 100), {});
    assert(Date.now() - started < 2000);
    assert.deepEqual(await readEnvironmentFromLoginShell(join(directory, 'missing'), env), {});
    assert.deepEqual(await readEnvironmentFromLoginShell(noisy, env), {});
    assert.equal(await hydratePosixPath(env, 'linux', [broken, working]), true);
    assert.equal(env.PATH, '/custom:/usr/bin:/bin');
    assert.equal(env.JAVA_HOME, '/existing');
  } finally { rmSync(directory, { recursive: true, force: true }); }
});

import { test, type TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { createHash, generateKeyPairSync, verify } from 'node:crypto';
import { chmodSync, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, utimesSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { gunzipSync } from 'node:zlib';
import { parseRelease } from '../scripts/release-lib.mjs';
const { buildRelease, assertNativeDependencies, githubRepository } = await import(new URL('../scripts/release.mjs', import.meta.url).href);

function fixture() {
  const dir = mkdtempSync(join(tmpdir(), 'release-gates-'));
  const git = (...args: string[]) => execFileSync('git', args, { cwd: dir, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
  git('init', '-b', 'release');
  writeFileSync(join(dir, '.gitignore'), 'releases/\n');
  mkdirSync(join(dir, 'app'));
  writeFileSync(join(dir, 'app/package.json'), JSON.stringify({ version: '1.0.0', type: 'module' }));
  mkdirSync(join(dir, 'app/dist')); writeFileSync(join(dir, 'app/dist/index.html'), 'Fixture UI\n');
  git('add', '.');
  git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-m', 'Fixture');
  git('branch', 't3/shipped');
  return { dir, git, options: { source: join(dir, 'app'), releases: join(dir, 'releases'), repository: dir, build: false, install: false } };
}

test('release refuses claimed branch tip omitted from release, then records merged issue numbers', () => {
  const f = fixture();
  try {
    f.git('switch', 't3/shipped');
    writeFileSync(join(f.dir, 'feature'), 'feature');
    f.git('add', '.');
    f.git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-m', 'Feature');
    f.git('switch', 'release');
    const options = { ...f.options, issues: [30], readIssueComments: (issue: number) => {
      assert.equal(issue, 30);
      return [{ body: 'Claimed by AT1 (Codex), branch t3/shipped' }];
    } };
    assert.throws(() => buildRelease(options), /#30.*t3\/shipped.*merge/i);
    assert.equal(existsSync(f.options.releases), false);
    f.git('merge', '--ff-only', 't3/shipped');
    const info = buildRelease(options);
    assert.deepEqual(info.issues, [30]);
    assert.equal(info.commit, f.git('rev-parse', 'HEAD'));
    assert.deepEqual(JSON.parse(readFileSync(join(f.options.releases, info.id, 'release.json'), 'utf8')).issues, [30]);
    assert.deepEqual(JSON.parse(readFileSync(join(f.options.releases, 'feed/latest.json'), 'utf8')).issues, [30]);
  } finally { rmSync(f.dir, { recursive: true, force: true }); }
});

for (const [name, comments, message] of [
  ['missing claim', [{ body: 'Ready for release' }], /#31.*no claim.*Add/],
  ['missing local branch', [{ body: 'Claimed by RG1, branch t3/missing' }], /#31.*t3\/missing.*missing.*Restore/],
  ['any claimed branch unmerged', [{ body: 'Claimed by RG1, branch t3/missing' }, { body: 'Claimed by RG2, branch t3/shipped' }], /#31.*t3\/missing.*missing.*Restore/],
] as const) test(`release fails before publication on ${name}`, () => {
  const f = fixture();
  try {
    assert.throws(() => buildRelease({ ...f.options, issues: [31], readIssueComments: () => comments }), message);
    assert.equal(existsSync(f.options.releases), false);
  } finally { rmSync(f.dir, { recursive: true, force: true }); }
});

test('repeated same-branch claims pass; every issue is checked; invalid issues fail closed', () => {
  const f = fixture();
  try {
    const readIssueComments = () => [{ body: 'Claimed by RG1, branch t3/shipped' }, { body: 'Claimed by RG1, branch t3/shipped' }];
    const info = buildRelease({ ...f.options, issues: [30, 31, 30], readIssueComments });
    assert.deepEqual(info.issues, [30, 31]);
    for (const issues of [null, ['30'], [0], [30, 30]]) assert.throws(() => parseRelease({ ...info, issues }), /Invalid release issues/);
    const { issues: _issues, ...legacy } = info;
    assert.doesNotThrow(() => parseRelease(legacy));
    for (const issues of [[0], [-1], [1.5], [Number.MAX_SAFE_INTEGER + 1]]) assert.throws(() => buildRelease({ ...f.options, issues }), /Invalid issues/);
    assert.throws(() => buildRelease({ ...f.options, issues: [30, 31], readIssueComments: (issue: number) => issue === 30 ? readIssueComments() : [] }), /#31.*no claim/);
    assert.throws(() => buildRelease({ ...f.options, issues: [30], readIssueComments: () => { throw new Error('offline'); } }), /#30.*cannot read.*gh authentication/);
  } finally { rmSync(f.dir, { recursive: true, force: true }); }
});

test('release without issues warns loudly', t => {
  const f = fixture();
  const warn = t.mock.method(console, 'warn', () => {});
  try {
    assert.deepEqual(buildRelease(f.options).issues, []);
    assert.match(warn.mock.calls[0].arguments[0], /WARNING: RELEASE ANCESTRY GATE SKIPPED.*--issues/);
  } finally { rmSync(f.dir, { recursive: true, force: true }); }
});

test('Rust resource failure prevents publication; successful staging retains UI and notices', () => {
  const f = fixture(), bin = mkdtempSync(join(tmpdir(), 'release-cargo-')), previousPath = process.env.PATH, previousTarget = process.env.CARGO_TARGET_DIR;
  try {
    // A caller's shared target directory once made 0.13.2 publish the previous build's binaries.
    process.env.CARGO_TARGET_DIR = join(bin, 'elsewhere');
    for (const directory of ['scripts', 'crates/minarbot-server', 'crates/minarbot-launcher', 'target/release']) mkdirSync(join(f.dir, directory), { recursive: true });
    writeFileSync(join(f.dir, 'Cargo.toml'), '[workspace]\n');
    writeFileSync(join(f.dir, 'crates/minarbot-server/provenance.json'), '{}');
    writeFileSync(join(f.dir, 'target/release/minarbot-server'), 'fixture binary', { mode: 0o700 });
    writeFileSync(join(f.dir, 'target/release/minarbot-launcher'), 'fixture launcher', { mode: 0o700 });
    writeFileSync(join(f.dir, 'crates/minarbot-launcher/provenance.json'), '{"launcher":true}');
    writeFileSync(join(f.dir, 'crates/minarbot-launcher/NOTICE'), 'T3 MIT notice');
    for (const name of ['LICENSE', 'agent-delegation.LICENSE']) writeFileSync(join(f.dir, 'app', name), 'Fixture notice');
    writeFileSync(join(f.dir, 'app/package.json'), JSON.stringify({ version: '1.0.0', type: 'module', scripts: { lint: 'node -e "0"', build: 'node -e "0"', test: 'node -e "0"' } }));
    writeFileSync(join(bin, 'cargo'), `#!${process.execPath}\nimport { appendFileSync } from 'node:fs';\nappendFileSync('cargo-checks', JSON.stringify(process.argv.slice(2)) + '\\n');\nappendFileSync('cargo-targets', process.env.CARGO_TARGET_DIR + '\\n');\n`, { mode: 0o700 });
    writeFileSync(join(bin, 'ldd'), `#!${process.execPath}
import { appendFileSync, existsSync, readFileSync } from 'node:fs';
appendFileSync(${JSON.stringify(join(f.dir, 'ldd-checks'))}, process.argv[2] + '\\n');
const mode = ${JSON.stringify(join(f.dir, 'dependency-mode'))};
const value = existsSync(mode) ? readFileSync(mode, 'utf8') : 'system';
console.log(value === 'missing' ? 'libvips.so => not found' : value === 'external' ? 'libvips.so => /tmp/node_modules/libvips.so (0x1)' : 'libc.so.6 => /lib/x86_64-linux-gnu/libc.so.6 (0x1)\\n/lib64/ld-linux-x86-64.so.2 (0x2)');
`, { mode: 0o700 });
    writeFileSync(join(f.dir, 'scripts/resource-gate.mjs'), "import { existsSync } from 'node:fs';\nif (existsSync('gate-fails')) throw new Error('Rust resource budget exceeded');\n");
    writeFileSync(join(f.dir, 'scripts/production-env-gate.mjs'), "import { existsSync } from 'node:fs';\nif (existsSync('production-fails')) throw new Error('Production env startup failed');\n");
    writeFileSync(join(f.dir, 'scripts/budgets.json'), '{}');
    writeFileSync(join(f.dir, 'gate-fails'), 'fail');
    process.env.PATH = `${bin}:${previousPath}`;
    const options = { ...f.options, build: true, allowDirty: true };
    assert.throws(() => buildRelease(options), /resource-gate/);
    assert.equal(existsSync(f.options.releases), false);
    rmSync(join(f.dir, 'gate-fails'));
    writeFileSync(join(f.dir, 'production-fails'), 'fail');
    assert.throws(() => buildRelease(options), /production-env-gate/);
    assert.equal(existsSync(f.options.releases), false);
    rmSync(join(f.dir, 'production-fails'));
    for (const [mode, message] of [['missing', /Native dependency missing/], ['external', /outside system loader paths/]] as const) {
      writeFileSync(join(f.dir, 'dependency-mode'), mode);
      assert.throws(() => buildRelease(options), message);
      assert.equal(existsSync(join(f.options.releases, 'feed/latest.json')), false);
    }
    rmSync(join(f.dir, 'dependency-mode'));
    const info = buildRelease(options), stage = join(f.options.releases, info.id);
    const dependencies = readFileSync(join(f.dir, 'ldd-checks'), 'utf8').trim().split('\n');
    assert.equal(dependencies.length, 4);
    assert(dependencies.at(-2)!.endsWith('/bin/minarbot-server'));
    assert(dependencies.at(-1)!.endsWith('/bin/minarbot-launcher'));
    assert.equal(readFileSync(join(stage, 'dist/index.html'), 'utf8'), 'Fixture UI\n');
    assert.equal(readFileSync(join(stage, 'bin/minarbot-server'), 'utf8'), 'fixture binary');
    for (const name of ['LICENSE', 'agent-delegation.LICENSE']) assert.equal(readFileSync(join(stage, 'bin', name), 'utf8'), 'Fixture notice');
    assert.equal(readFileSync(join(stage, 'bin/provenance.json'), 'utf8'), '{}');
    assert.equal(readFileSync(join(stage, 'bin/minarbot-launcher'), 'utf8'), 'fixture launcher');
    assert.equal(readFileSync(join(stage, 'bin/launcher-provenance.json'), 'utf8'), '{"launcher":true}');
    assert.equal(readFileSync(join(stage, 'bin/launcher.NOTICE'), 'utf8'), 'T3 MIT notice');
    assert.equal(readFileSync(join(stage, 'machine/install-machine.sh'), 'utf8'), readFileSync(new URL('../scripts/install-machine.sh', import.meta.url), 'utf8'));
    const checks = readFileSync(join(f.dir, 'cargo-checks'), 'utf8').trim().split('\n').map(line => JSON.parse(line));
    assert.equal(checks.length, 20);
    assert(checks.some(args => args.join(' ') === 'build --release --locked'));
    assert.deepEqual([...new Set(readFileSync(join(f.dir, 'cargo-targets'), 'utf8').trim().split('\n'))], [join(f.dir, 'target')]);
  } finally {
    process.env.PATH = previousPath;
    if (previousTarget === undefined) delete process.env.CARGO_TARGET_DIR; else process.env.CARGO_TARGET_DIR = previousTarget;
    rmSync(f.dir, { recursive: true, force: true }); rmSync(bin, { recursive: true, force: true }); }
});

test('release CLI demonstrates passing and failing --issues using a fixture gh reader', t => {
  const f = fixture(), bin = mkdtempSync(join(tmpdir(), 'release-gh-'));
  try {
    mkdirSync(join(f.dir, 'scripts'), { recursive: true });
    for (const name of ['release.mjs', 'release-lib.mjs', 'install-machine.sh', 'uninstall-machine.sh']) cpSync(new URL(`../scripts/${name}`, import.meta.url), join(f.dir, 'scripts', name));
    cpSync(new URL('../scripts/release-lib.mjs', import.meta.url), join(f.dir, 'scripts/release-lib.mjs'));
    const pkg = { name: 'release-gate-fixture', version: '1.0.0', type: 'module', scripts: { lint: 'node -e "0"', test: 'node -e "0"', build: 'node -e "0"' } };
    writeFileSync(join(f.dir, 'app/package.json'), JSON.stringify(pkg));
    writeFileSync(join(f.dir, 'app/package-lock.json'), JSON.stringify({ name: pkg.name, version: pkg.version, lockfileVersion: 3, packages: { '': pkg } }));
    f.git('add', '.');
    f.git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-m', 'CLI fixture');
    f.git('switch', '-c', 't3/unmerged');
    writeFileSync(join(f.dir, 'unmerged-feature'), 'not shipped');
    f.git('add', '.');
    f.git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-m', 'Unmerged feature');
    f.git('switch', 'release');
    writeFileSync(join(bin, 'gh'), `#!${process.execPath}
import assert from 'node:assert/strict';
const args = process.argv.slice(2);
assert.deepEqual(args, ['issue', 'view', args[2], '-R', ${JSON.stringify(githubRepository)}, '--json', 'comments']);
assert(['30', '31'].includes(args[2]));
console.log(JSON.stringify({comments: [{body: 'Claimed by Fixture, branch ' + (args[2] === '30' ? 't3/shipped' : 't3/unmerged')}]}));
`, { mode: 0o700 });
    const run = (issues: string) => spawnSync(process.execPath, [join(f.dir, 'scripts/release.mjs'), '--no-github', '--issues', issues, '--releases', f.options.releases], { cwd: f.dir, encoding: 'utf8', env: { ...process.env, PATH: `${bin}:${process.env.PATH}`, npm_config_update_notifier: 'false' } });
    const failed = run('30,31');
    assert.equal(failed.status, 1);
    assert.match(failed.stderr, /#31.*t3\/unmerged.*Merge the branch tip/);
    assert.equal(existsSync(f.options.releases), false);
    t.diagnostic(`FAIL DEMO --issues 30,31 (exit ${failed.status}): ${failed.stderr.split('\n').find(line => line.startsWith('Error:'))}`);
    const passed = run('30');
    assert.equal(passed.status, 0, passed.stderr);
    assert.match(passed.stdout, /Published 1\.0\.0-/);
    const info = JSON.parse(readFileSync(join(f.options.releases, 'feed/latest.json'), 'utf8'));
    assert.deepEqual(info.issues, [30]);
    t.diagnostic(`PASS DEMO --issues 30 (exit ${passed.status}): ${passed.stdout.trim().split('\n').at(-1)}; metadata issues=${JSON.stringify(info.issues)}`);
  } finally { rmSync(f.dir, { recursive: true, force: true }); rmSync(bin, { recursive: true, force: true }); }
});

function githubFixture(t: TestContext, withImage = false) {
  const f = fixture();
  const sandbox = mkdtempSync(join(tmpdir(), 'release-github-'));
  t.after(() => { rmSync(f.dir, { recursive: true, force: true }); rmSync(sandbox, { recursive: true, force: true }); });
  const { publicKey, privateKey } = generateKeyPairSync('ed25519');
  const pem = privateKey.export({ type: 'pkcs8', format: 'pem' });
  const key = join(sandbox, 'signing.pem');
  writeFileSync(key, pem, { mode: 0o600 });
  mkdirSync(join(f.dir, 'scripts'), { recursive: true });
  for (const name of ['release-lib.mjs', 'install-machine.sh', 'uninstall-machine.sh']) {
    cpSync(new URL(`../scripts/${name}`, import.meta.url), join(f.dir, 'scripts', name));
  }
  const script = readFileSync(new URL('../scripts/release.mjs', import.meta.url), 'utf8');
  const rawPublicKey = publicKey.export({ type: 'spki', format: 'der' }).subarray(-32).toString('base64');
  writeFileSync(join(f.dir, 'scripts/release.mjs'), script.replace('kSIv+agT4RsABust/LmvPVjfWDxZxM0YKL03gts/UEM=', rawPublicKey));
  writeFileSync(join(f.dir, 'app/package.json'), JSON.stringify({ version: '1.0.0', scripts: {
    lint: 'node -e "process.exit(Number(process.env.FIXTURE_GATE_FAIL ?? 0))"', test: 'node -e "0"', build: 'node -e "0"',
  } }));
  mkdirSync(join(f.dir, 'desktop/scripts'), { recursive: true });
  writeFileSync(join(f.dir, 'desktop/scripts/build.mjs'), `
import { writeFileSync } from 'node:fs';
import { join } from 'node:path';
const [version, out] = process.argv.slice(2);
const name = 'Minarbot-' + version + '-x86_64.AppImage';
writeFileSync(join(out, name), 'AppImage fixture ' + version);
writeFileSync(join(out, 'latest-linux.yml'), 'version: ' + version + '\\npath: ' + name + '\\n');
`);
  if (withImage) {
    mkdirSync(join(f.dir, 'computer/image'), { recursive: true });
    mkdirSync(join(f.dir, 'crates/minarbot-server'), { recursive: true });
    writeFileSync(join(f.dir, 'crates/minarbot-server/whatsapp-rust.LICENSE'), 'Fixture WhatsApp notice');
    mkdirSync(join(f.dir, 'target/release'), { recursive: true });
    writeFileSync(join(f.dir, 'computer/image/Containerfile'), 'FROM scratch');
    writeFileSync(join(f.dir, 'target/release/minarbot-server'), 'Fixture supervisor');
    mkdirSync(join(f.dir, 'app/t3'));
    for (const file of ['LICENSE', 'agent-delegation.LICENSE', 't3/NOTICE']) writeFileSync(join(f.dir, 'app', file), 'Fixture notice');
    writeFileSync(join(sandbox, 'podman'), `#!${process.execPath}
import assert from 'node:assert/strict';
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
const [remote, command, ...args] = process.argv.slice(2);
assert.equal(remote, '--remote=false');
assert.equal(process.env.MINARBOT_GHCR_TOKEN, undefined);
if (command === 'info') console.log('true');
else if (command === 'login') {
  assert.deepEqual(args.slice(2), ['--username', 'fixture', '--password-stdin', 'ghcr.io']);
  assert.equal(readFileSync(0, 'utf8'), 'fixture-token\\n');
} else if (command === 'build') {
  assert.equal(readFileSync(join(args.at(-1), 'minarbot-server'), 'utf8'), 'Fixture supervisor');
  assert.equal(readFileSync(join(args.at(-1), 'whatsapp-rust.LICENSE'), 'utf8'), 'Fixture WhatsApp notice');
  assert.match(args.at(-2), /^ghcr\\.io\\/labsiqbal\\/minarbot-computer:[a-f0-9]{40}$/);
} else {
  assert.equal(command, 'push');
  assert.equal(args[2], '--digestfile');
  writeFileSync(args[3], 'sha256:' + 'a'.repeat(64) + '\\n');
}
`, { mode: 0o700 });
  }
  f.git('add', '.');
  f.git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-m', 'Internal notes never go public');
  const output = join(sandbox, 'uploaded');
  const commit = f.git('rev-parse', 'HEAD');
  mkdirSync(output);
  writeFileSync(join(sandbox, 'gh'), `#!${process.execPath}
import assert from 'node:assert/strict';
import { hash } from 'node:crypto';
import { appendFileSync, copyFileSync, existsSync, readFileSync, writeFileSync } from 'node:fs';
import { basename, join } from 'node:path';
const args = process.argv.slice(2);
const [command, action, tag] = args;
const out = ${JSON.stringify(output)};
const commit = ${JSON.stringify(commit)};
appendFileSync(join(out, 'calls'), JSON.stringify(args) + '\\n');
const state = join(out, 'state');
let release = existsSync(state) ? JSON.parse(readFileSync(state, 'utf8')) : undefined;
if (command === 'api') {
  assert.deepEqual(args, ['api', ${JSON.stringify(`repos/${githubRepository}/git/matching-refs/tags/v1.0.0`)}]);
  const sha = process.env.FIXTURE_WRONG_TAG ? '0'.repeat(40) : commit;
  console.log(JSON.stringify(release && !release.isDraft ? [{ ref: 'refs/tags/v1.0.0', object: { type: 'commit', sha } }] : []));
  process.exit(0);
}
assert.equal(command, 'release');
assert.equal(tag, 'v1.0.0');
assert.deepEqual(args.slice(3, 5), ['-R', ${JSON.stringify(githubRepository)}]);
if (action === 'view') {
  if (!release) { console.error('release not found'); process.exit(1); }
  console.log(JSON.stringify(release));
  process.exit(0);
}
if (action === 'download') {
  assert.equal(args[5], '--dir');
  for (let i = 7; i < args.length; i += 2) {
    assert.equal(args[i], '--pattern');
    copyFileSync(join(out, args[i + 1]), join(args[6], args[i + 1]));
  }
  process.exit(0);
}
const ciDraft = action === 'create' && args[5] === '--draft';
if (action === 'create' || action === 'upload') {
  if (action === 'create') {
    assert.equal(release, undefined);
    assert.deepEqual(args.slice(ciDraft ? 6 : 5, ciDraft ? 12 : 11), ['--target', commit, '--title', 'Minarbot 1.0.0', '--notes', 'Public notes']);
    release = { isDraft: true, targetCommitish: commit, name: 'Minarbot 1.0.0', body: 'Public notes', assets: [] };
  }
  assert.equal(release.isDraft, true);
  if (ciDraft) writeFileSync(state, JSON.stringify(release));
  for (const [i, path] of args.slice(action === 'create' ? ciDraft ? 12 : 11 : 5).entries()) {
    const name = basename(path);
    assert(!release.assets.some(asset => asset.name === name), 'duplicate upload');
    if (i === 1 && process.env.FIXTURE_UPLOAD_FAIL) {
      release.assets.push({ name, state: 'starter' });
      writeFileSync(state, JSON.stringify(release));
      console.error('fixture upload offline');
      process.exit(1);
    }
    copyFileSync(path, join(out, name));
    release.assets.push({ name, state: 'uploaded', digest: 'sha256:' + hash('sha256', readFileSync(path)) });
    writeFileSync(state, JSON.stringify(release));
  }
  if (action === 'create' && !ciDraft) writeFileSync(state, JSON.stringify({ ...release, isDraft: false }));
} else if (action === 'delete-asset') {
  const name = args[5];
  assert.equal(release.isDraft, true);
  assert.equal(release.assets.find(asset => asset.name === name).state, 'starter');
  release.assets = release.assets.filter(asset => asset.name !== name);
  writeFileSync(state, JSON.stringify(release));
} else {
  assert.equal(action, 'edit');
  assert.deepEqual(args.slice(5), release.assets.some(asset => asset.name === 'ci-checksums.json') ? ['--draft=false', '--latest'] : ['--draft=false']);
  assert.equal(release.isDraft, true);
  assert(release.assets.length >= 3);
  writeFileSync(state, JSON.stringify({ ...release, isDraft: false }));
}
`, { mode: 0o700 });
  const run = (args: string[], env: NodeJS.ProcessEnv = {}) => spawnSync(process.execPath, [
    join(f.dir, 'scripts/release.mjs'), ...(args.some(arg => arg.endsWith('-ci')) ? [] : ['--releases', f.options.releases]), ...args,
  ], { cwd: f.dir, encoding: 'utf8', env: { ...process.env, PATH: `${sandbox}:${process.env.PATH}`, HOME: sandbox, MINARBOT_RELEASE_SIGNING_KEY: key, MINARBOT_GHCR_USER: '', MINARBOT_GHCR_TOKEN: '', ...env } });
  const calls = () => existsSync(join(output, 'calls')) ? readFileSync(join(output, 'calls'), 'utf8').trim().split('\n').map(line => JSON.parse(line) as string[]) : [];
  return { ...f, run, calls, output, key, publicKey, commit, id: `1.0.0-${commit.slice(0, 8)}` };
}

test('release CLI uploads exact signed ustar assets and the version-matched desktop build', t => {
  const f = githubFixture(t);
  const result = f.run(['--desktop', '--no-image', '--notes', 'Public notes', '--signing-key', f.key], { MINARBOT_RELEASE_SIGNING_KEY: '/wrong/path' });
  assert.equal(result.status, 0, result.stderr);
  const uploaded = (name: string) => readFileSync(join(f.output, name));
  const archive = 'minarbot-1.0.0-linux-x86_64.tar.gz';
  assert.deepEqual(f.calls().find(args => args[1] === 'create')?.slice(11).map(path => path.split('/').at(-1)).sort(), [
    archive, 'latest.json', 'latest.json.sig', 'Minarbot-1.0.0-x86_64.AppImage', 'latest-linux.yml',
  ].sort());
  const tar = gunzipSync(uploaded(archive));
  assert.equal(tar.subarray(257, 265).toString(), 'ustar\0' + '00');
  assert.equal(execFileSync('tar', ['-xOzf', join(f.output, archive), './dist/index.html'], { encoding: 'utf8' }), 'Fixture UI\n');
  const manifest = uploaded('latest.json');
  const signature = uploaded('latest.json.sig').toString();
  assert.match(signature, /^[A-Za-z0-9+/]{86}==$/);
  assert(verify(null, manifest, f.publicKey, Buffer.from(signature, 'base64')));
  assert.deepEqual(JSON.parse(manifest.toString()), {
    version: '1.0.0', id: f.id, commit: f.commit, archive, sha256: createHash('sha256').update(uploaded(archive)).digest('hex'),
    size: uploaded(archive).length, notes: 'Public notes', created: JSON.parse(readFileSync(join(f.options.releases, 'feed/latest.json'), 'utf8')).created,
  });
  assert.match(uploaded('latest-linux.yml').toString(), /path: Minarbot-1\.0\.0-x86_64\.AppImage/);
});

test('release gates image publishing on GHCR credentials and signs the pushed digest', t => {
  const f = githubFixture(t, true), args = ['--notes', 'Public notes'];
  const noCredentials = f.run(args);
  assert.equal(noCredentials.status, 1);
  assert.match(noCredentials.stderr, /MINARBOT_GHCR_TOKEN.*--no-image/);
  assert.equal(existsSync(f.options.releases), false);
  assert.deepEqual(f.calls(), []);
  const result = f.run(args, { MINARBOT_GHCR_USER: 'fixture', MINARBOT_GHCR_TOKEN: 'fixture-token' });
  assert.equal(result.status, 0, result.stderr);
  const bytes = readFileSync(join(f.output, 'computer-image.json'));
  const signature = readFileSync(join(f.output, 'computer-image.json.sig'), 'utf8');
  assert(verify(null, bytes, f.publicKey, Buffer.from(signature, 'base64')));
  const legacy = JSON.parse(readFileSync(join(f.output, 'latest.json'), 'utf8'));
  assert.equal(legacy.computer_image, undefined);
  assert.deepEqual(JSON.parse(bytes.toString()), { ...legacy, computer_image: 'sha256:' + 'a'.repeat(64) });
});

test('CI draft and signing retain multi-platform assets and publish a signed Computer digest', t => {
  const f = githubFixture(t, true);
  const directory = mkdtempSync(join(tmpdir(), 'minarbot-ci-fixture-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const stage = join(directory, 'stage'), artifacts = join(directory, 'artifacts');
  mkdirSync(join(stage, 'bin'), { recursive: true });
  mkdirSync(artifacts);
  cpSync(join(f.dir, 'target/release/minarbot-server'), join(stage, 'bin/minarbot-server'));
  const archive = 'minarbot-1.0.0-linux-x86_64.tar.gz';
  execFileSync('tar', ['-czf', join(artifacts, archive), '-C', stage, '.']);
  for (const name of [
    'minarbot-1.0.0-linux-aarch64.tar.gz', 'minarbot-1.0.0-macos-x86_64.tar.gz', 'minarbot-1.0.0-macos-aarch64.tar.gz',
    'Minarbot-1.0.0-x86_64.AppImage', 'Minarbot-1.0.0-arm64.AppImage', 'latest-linux.yml', 'latest-linux-arm64.yml',
    'Minarbot-1.0.0-macos-arm64.dmg', 'Minarbot-1.0.0-macos-x64.dmg',
    'Minarbot-1.0.0-windows-x64.exe', 'Minarbot-1.0.0-windows-x64.exe.blockmap', 'latest.yml',
  ]) writeFileSync(join(artifacts, name), name);
  const draft = f.run(['--draft-ci', artifacts, '--notes', 'Public notes'], { MINARBOT_GHCR_USER: 'fixture', MINARBOT_GHCR_TOKEN: 'fixture-token' });
  assert.equal(draft.status, 0, draft.stderr);
  const signed = f.run(['--sign-ci', '1.0.0']);
  assert.equal(signed.status, 0, signed.stderr);
  const readSigned = (name: string) => {
    const bytes = readFileSync(join(f.output, name));
    assert(verify(null, bytes, f.publicKey, Buffer.from(readFileSync(join(f.output, name + '.sig'), 'utf8'), 'base64')));
    return JSON.parse(bytes.toString());
  };
  const latest = readSigned('latest.json');
  assert.equal(latest.archive, archive);
  assert.deepEqual(Object.keys(latest.assets).sort(), readdirSync(artifacts).filter(name => name !== 'ci-checksums.json').sort());
  assert.deepEqual(readSigned('computer-image.json'), { ...latest, computer_image: 'sha256:' + 'a'.repeat(64) });
});

test('release CLI keeps a failed upload local and resumes the draft idempotently without rebuilding', t => {
  const f = githubFixture(t);
  const failed = f.run(['--desktop', '--no-image', '--notes', 'Public notes'], { FIXTURE_UPLOAD_FAIL: '1' });
  assert.equal(failed.status, 1);
  assert.match(failed.stderr, /fixture upload offline/);
  assert.match(failed.stderr, new RegExp(`Local release ${f.id}[\\s\\S]*--github-only ${f.id}`));
  const feed = join(f.options.releases, 'feed/latest.json');
  const local = readFileSync(feed, 'utf8');
  assert.equal(JSON.parse(local).id, f.id);
  utimesSync(join(f.options.releases, f.id, 'dist/index.html'), 1, 1);
  const retry = () => f.run(['--github-only', f.id], { FIXTURE_GATE_FAIL: '1' });
  for (const _ of [1, 2]) {
    const result = retry();
    assert.equal(result.status, 0, result.stderr);
    assert.equal(readFileSync(feed, 'utf8'), local);
  }
  const conflict = f.run(['--github-only', f.id], { FIXTURE_WRONG_TAG: '1' });
  assert.equal(conflict.status, 1);
  assert.match(conflict.stderr, /GitHub tag v1\.0\.0 does not point to release commit/);
  const calls = f.calls();
  assert.equal(calls.filter(args => args[1] === 'create').length, 1);
  assert.equal(calls.filter(args => args[1] === 'edit').length, 1);
  assert.deepEqual(calls.filter(args => args[1] === 'delete-asset').map(args => args[5]), ['latest.json']);
  assert.equal(calls.filter(args => args[1] === 'upload').length, 1);
  assert(calls.findLast(args => args[1] === 'upload')?.some(arg => arg.endsWith('Minarbot-1.0.0-x86_64.AppImage')));
});

test('release CLI skips GitHub for local-only releases and blocks dirty trees, failed gates, and implicit notes', t => {
  const f = githubFixture(t);
  const release = ['--no-image', '--notes', 'Public notes'];
  writeFileSync(join(f.dir, 'dirty'), 'dirty');
  assert.match(f.run(release).stderr, /dirty worktree/);
  rmSync(join(f.dir, 'dirty'));
  assert.match(f.run([]).stderr, /--notes/);
  assert.equal(f.run(release, { FIXTURE_GATE_FAIL: '7' }).status, 1);
  assert.equal(existsSync(f.options.releases), false);
  const result = f.run(['--no-github'], { MINARBOT_RELEASE_SIGNING_KEY: '/wrong/path' });
  assert.equal(result.status, 0, result.stderr);
  assert(existsSync(join(f.options.releases, f.id, 'release.json')));
  assert.equal(f.run(['--github-only', f.id]).status, 1);
  assert.deepEqual(f.calls(), []);
});

test('release CLI refuses wrong-mode, non-Ed25519 and mismatched signing keys before GitHub access', t => {
  const f = githubFixture(t);
  chmodSync(f.key, 0o640);
  const first = f.run(['--no-image', '--notes', 'Public notes']);
  assert.equal(first.status, 1);
  assert.match(first.stderr, /mode 0600/);
  chmodSync(f.key, 0o600);
  for (const [algorithm, message] of [['x25519', /Ed25519 private key/], ['ed25519', /does not match.*public key/]] as const) {
    const { privateKey } = algorithm === 'x25519' ? generateKeyPairSync('x25519') : generateKeyPairSync('ed25519');
    writeFileSync(f.key, privateKey.export({ type: 'pkcs8', format: 'pem' }));
    const result = f.run(['--github-only', f.id]);
    assert.equal(result.status, 1);
    assert.match(result.stderr, message);
  }
  assert.deepEqual(f.calls(), []);
});

const releaseRoot = new URL('../target/release/', import.meta.url);
test('staged Rust binaries resolve only system loader libraries', { skip: !existsSync(new URL('minarbot-server', releaseRoot)) }, () => {
  const stage = mkdtempSync(join(tmpdir(), 'minarbot-native-dependencies-'));
  try {
    mkdirSync(join(stage, 'bin'));
    for (const name of ['minarbot-server', 'minarbot-launcher']) {
      const binary = join(stage, 'bin', name);
      cpSync(new URL(name, releaseRoot), binary);
      assertNativeDependencies(binary);
    }
  } finally { rmSync(stage, { recursive: true, force: true }); }
});

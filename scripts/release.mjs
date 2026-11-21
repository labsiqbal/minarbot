#!/usr/bin/env node
import { execFileSync, spawnSync } from 'node:child_process';
import { createPrivateKey, createPublicKey, createHash, sign, verify } from 'node:crypto';
import { closeSync, constants, cpSync, createReadStream, existsSync, fstatSync, mkdirSync, mkdtempSync, openSync, readFileSync, realpathSync, renameSync, rmSync, statSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { basename, dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';
import { durableWrite, fileManifest, parseRelease, syncDirectory, verifyRelease } from './release-lib.mjs';
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const releasePublicKey = 'kSIv+agT4RsABust/LmvPVjfWDxZxM0YKL03gts/UEM=';
export const githubRepository = 'labsiqbal/minarbot';
const computerRepository = 'ghcr.io/labsiqbal/minarbot-computer';

function publishComputerImage(commit, user, token) {
  const stage = mkdtempSync(join(tmpdir(), 'minarbot-computer-'));
  try {
    const env = { ...process.env, DOCKER_CONFIG: stage };
    delete env.MINARBOT_GHCR_TOKEN;
    const runtime = spawnSync('podman', ['--remote=false', 'info', '--format', '{{.Host.Security.Rootless}}'], { env, encoding: 'utf8' }).stdout?.trim() === 'true' ? 'podman' : 'docker';
    const authFile = join(stage, 'auth.json');
    const auth = runtime === 'podman' ? ['--authfile', authFile] : [];
    const prefix = runtime === 'podman' ? ['--remote=false'] : [];
    const run = (args, options) => execFileSync(runtime, [...prefix, ...args], { env, ...options });
    const tag = `${computerRepository}:${commit}`;
    const context = join(stage, 'context');
    mkdirSync(context);
    cpSync(join(root, 'computer/image/Containerfile'), join(context, 'Containerfile'));
    cpSync(join(root, 'target/release/minarbot-server'), join(context, 'minarbot-server'));
    for (const file of ['LICENSE', 'agent-delegation.LICENSE']) cpSync(join(root, 'app', file), join(context, file));
    cpSync(join(root, 'app/t3/NOTICE'), join(context, 'NOTICE'));
    run(['login', ...auth, '--username', user, '--password-stdin', 'ghcr.io'], {
      input: `${token}\n`, stdio: ['pipe', 'ignore', 'ignore'],
    });
    run(['build', '--platform', 'linux/amd64', '-f', join(context, 'Containerfile'), '-t', tag, context], { stdio: 'inherit' });
    const digestFile = join(stage, 'digest');
    let digest;
    if (runtime === 'podman') {
      run(['push', ...auth, '--digestfile', digestFile, tag], { stdio: 'inherit' });
      digest = readFileSync(digestFile, 'utf8').trim();
    } else {
      digest = /digest: (sha256:[a-f0-9]{64}) size: \d+/.exec(run(['push', tag], { encoding: 'utf8' }))?.[1];
    }
    if (!/^sha256:[a-f0-9]{64}$/.test(digest)) throw new Error('GHCR push did not return a Computer image digest.');
    return digest;
  } finally {
    rmSync(stage, { recursive: true, force: true });
  }
}

export function readIssueComments(issue) {
  return JSON.parse(execFileSync('gh', ['issue', 'view', String(issue), '-R', githubRepository, '--json', 'comments'], { encoding: 'utf8' })).comments;
}

export function assertNativeDependencies(binary) {
  const output = execFileSync('ldd', [binary], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
  if (/not found/i.test(output)) throw new Error(`Native dependency missing for ${binary}: ${output}`);
  for (const line of output.split('\n')) {
    const match = line.match(/=>\s+(\/[^\s(]+)/) ?? line.match(/^\s*(\/[^\s(]+)/);
    let path; if (match) { try { path = realpathSync(match[1]); } catch { path = match[1]; } }
    if (path && !['/lib', '/usr/lib', '/lib64'].some(root => path === root || path.startsWith(`${root}/`))) {
      throw new Error(`Native dependency outside system loader paths for ${binary}: ${match[1]}`);
    }
  }
}

function signingKey(path) {
  const fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    const stat = fstatSync(fd);
    if (!stat.isFile() || (stat.mode & 0o7777) !== 0o600) throw new Error('Signing key must be a regular file with mode 0600.');
    let key;
    try { key = createPrivateKey(readFileSync(fd)); }
    catch { throw new Error('Could not read an Ed25519 private key from the signing key file.'); }
    if (key.asymmetricKeyType !== 'ed25519') throw new Error('Signing key must be an Ed25519 private key.');
    return key;
  } finally { closeSync(fd); }
}

async function sha256(path) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest('hex');
}

const publicationDirectory = (releases, id) => join(releases, '.github', id);
function checkCleanTree() {
  if (execFileSync('git', ['status', '--porcelain'], { cwd: root, encoding: 'utf8' }).trim()) {
    throw new Error('Refusing dirty worktree. Use --allow-dirty for a local development release.');
  }
}

function github(args, cwd = root) {
  const result = spawnSync('gh', args, { encoding: 'utf8', cwd });
  if (result.error) throw result.error;
  if (result.status === 0) return result.stdout.trim();
  const error = result.stderr.trim();
  if (args[1] === 'view' && result.status === 1 && error === 'release not found') return undefined;
  throw new Error(`gh ${args[0]} ${args[1]} failed: ${error || result.signal || `exit ${result.status}`}`);
}

async function signedRuntimeAssets(releases, directory, info, keyPath) {
  const { version, id, commit, notes, created } = info;
  const archive = `minarbot-${version}-linux-x86_64.tar.gz`;
  const names = [archive, 'latest.json', 'latest.json.sig'];
  const bundle = join(directory, 'signed');
  const cached = existsSync(bundle);
  const key = signingKey(keyPath);
  const stage = cached ? bundle : mkdtempSync(join(directory, '.signing-'));
  try {
    const archiveFile = join(stage, archive);
    if (!cached) execFileSync('tar', ['--format=ustar', '-czf', archiveFile, '-C', join(releases, id), '.'], { stdio: 'inherit' });
    const archiveHash = await sha256(archiveFile);
    const manifest = { version, id, commit, archive, sha256: archiveHash, size: statSync(archiveFile).size, notes, created };
    const publicKey = createPublicKey({ format: 'jwk', key: { kty: 'OKP', crv: 'Ed25519', x: Buffer.from(releasePublicKey, 'base64').toString('base64url') } });
    function writeSigned(name, value) {
      const manifestFile = join(stage, name), signatureFile = join(stage, `${name}.sig`);
      const bytes = Buffer.from(JSON.stringify(value)), signature = sign(null, bytes, key);
      if (!verify(null, bytes, publicKey, signature)) throw new Error('Signing key does not match the Minarbot release public key.');
      const encoded = signature.toString('base64');
      if (cached) {
        if (!readFileSync(manifestFile).equals(bytes) || readFileSync(signatureFile, 'utf8') !== encoded) throw new Error('Cached signed release assets changed.');
      } else {
        durableWrite(manifestFile, bytes);
        durableWrite(signatureFile, encoded);
      }
    }
    writeSigned('latest.json', manifest);
    if (info.computerImage) {
      // Existing installations reject unknown fields in latest.json.
      names.push('computer-image.json', 'computer-image.json.sig');
      writeSigned('computer-image.json', { ...manifest, computer_image: info.computerImage });
    }
    if (!cached) {
      renameSync(stage, bundle);
      syncDirectory(directory);
    }
    return { files: names.map(name => join(bundle, name)), archiveHash };
  } finally {
    if (!cached) rmSync(stage, { recursive: true, force: true });
  }
}

async function publishGithub(releases, id, keyPath) {
  const info = verifyRelease(releases, id);
  const { version, commit, notes } = info;
  const directory = publicationDirectory(releases, id);
  const readyFile = join(directory, 'ready.json');
  if (!existsSync(readyFile)) throw new Error('This local release has no completed public release build with explicit --notes.');
  const ready = JSON.parse(readFileSync(readyFile, 'utf8'));
  if (ready.release !== await sha256(join(releases, id, 'release.json'))) throw new Error('Local release metadata changed after the release build.');
  const desktop = ready.desktop ? desktopAssets(directory, version) : [];
  const desktopHashes = await Promise.all(desktop.map(sha256));
  if (ready.desktop && !desktopHashes.every((hash, i) => hash === ready.desktop[i])) throw new Error('Local desktop assets changed after the release build.');
  const { files, archiveHash } = await signedRuntimeAssets(releases, directory, info, keyPath);
  const tag = `v${version}`, title = `Minarbot ${version}`, repo = ['-R', githubRepository];
  const hashes = [archiveHash, ...await Promise.all(files.slice(1).map(sha256)), ...desktopHashes];
  const assets = [...files, ...desktop].map((file, i) => ({ file, name: basename(file), digest: `sha256:${hashes[i]}` }));
  const paths = list => list.map(asset => relative(directory, asset.file));
  const existing = github(['release', 'view', tag, ...repo, '--json', 'assets,body,isDraft,name,targetCommitish']);
  const release = existing === undefined ? undefined : JSON.parse(existing);
  // GitHub ignores --target when a tag already exists.
  const endpoint = `repos/${githubRepository}/git`;
  const refs = JSON.parse(github(['api', `${endpoint}/matching-refs/tags/${tag}`]));
  let target = refs.find(ref => ref.ref === `refs/tags/${tag}`)?.object;
  while (target?.type === 'tag') target = JSON.parse(github(['api', `${endpoint}/tags/${target.sha}`, '--jq', '.object']));
  if ((target && (target.type !== 'commit' || target.sha !== commit)) || (!target && release && !release.isDraft)) {
    throw new Error(`GitHub tag ${tag} does not point to release commit ${commit}.`);
  }
  if (!release) {
    checkCleanTree();
    // With asset arguments, gh uploads to a draft and publishes after all uploads pass.
    github(['release', 'create', tag, ...repo, '--target', commit, '--title', title, '--notes', notes, ...paths(assets)], directory);
  } else {
    if (release.name !== title || release.body !== notes || (!target && release.targetCommitish !== commit)) {
      throw new Error(`GitHub release ${tag} has different release metadata.`);
    }
    const remoteAssets = new Map((release.assets ?? []).map(asset => [asset.name, asset]));
    if ([...remoteAssets.keys()].some(name => !assets.some(asset => asset.name === name))) throw new Error(`GitHub release ${tag} has unexpected assets.`);
    const pending = [];
    for (const asset of assets) {
      const remote = remoteAssets.get(asset.name);
      if (remote?.state === 'uploaded' && remote.digest === asset.digest) continue;
      if (!release.isDraft || (remote && remote.state !== 'starter')) throw new Error(`GitHub release ${tag} has a missing or different asset: ${asset.name}.`);
      pending.push(asset);
    }
    if (release.isDraft) {
      checkCleanTree();
      for (const { name } of pending) {
        if (remoteAssets.has(name)) github(['release', 'delete-asset', tag, ...repo, name, '--yes']);
      }
      if (pending.length) github(['release', 'upload', tag, ...repo, ...paths(pending)], directory);
      checkCleanTree();
      github(['release', 'edit', tag, ...repo, '--draft=false']);
    }
  }
  console.log(`Published ${tag} to ${githubRepository}`);
}

function desktopAssets(directory, version) {
  const name = `Minarbot-${version}-x86_64.AppImage`;
  const image = join(directory, name), metadata = join(directory, 'latest-linux.yml');
  // ponytail: read electron-builder's fixed version and path lines. Parse YAML if accepting metadata from other builders.
  const yaml = readFileSync(metadata, 'utf8');
  if (/^version: (.+)$/m.exec(yaml)?.[1] !== version || /^path: (.+)$/m.exec(yaml)?.[1] !== name || !statSync(image).isFile()) {
    throw new Error('Desktop build did not produce the matching AppImage and latest-linux.yml.');
  }
  return [image, metadata];
}

export function buildRelease({ source = join(root, 'app'), releases = join(homedir(), '.local/share/minarbot/releases'), allowDirty = false, notes, build = true, install = true, repository = root, issues = [], readIssueComments: readComments = readIssueComments, computerImage: buildImage } = {}) {
  const git = args => execFileSync('git', args, { cwd: repository, encoding: 'utf8' }).trim();
  if (!Array.isArray(issues) || issues.some(issue => !Number.isSafeInteger(issue) || issue <= 0)) throw new Error('Invalid issues. Use --issues 30,31 with positive issue numbers.');
  issues = [...new Set(issues)];
  if (!issues.length) console.warn('WARNING: RELEASE ANCESTRY GATE SKIPPED. Supply --issues 30,31 for every shipped issue.');
  if (!allowDirty && git(['status', '--porcelain'])) throw new Error('Refusing dirty worktree. Use --allow-dirty for a development release.');
  const { version } = JSON.parse(readFileSync(join(source, 'package.json'), 'utf8')), commit = git(['rev-parse', 'HEAD']);
  for (const issue of issues) {
    let comments;
    try { comments = readComments(issue); }
    catch { throw new Error(`Issue #${issue}: cannot read claim comments. Check gh authentication and access to ${githubRepository}, then retry.`); }
    if (!Array.isArray(comments) || comments.some(comment => typeof comment?.body !== 'string')) throw new Error(`Issue #${issue}: invalid comments. Fix the issue reader and retry.`);
    const branches = [...new Set(comments.flatMap(comment => [...comment.body.matchAll(/^Claimed by [^\r\n]+, branch (t3\/\S+)[ \t]*\r?$/gm)].map(match => match[1])))];
    if (!branches.length) throw new Error(`Issue #${issue}: no claim comment. Add "Claimed by <agent>, branch t3/<slug>" and retry.`);
    for (const branch of branches) {
      const ref = `refs/heads/${branch}`;
      try { git(['show-ref', '--verify', '--quiet', ref]); }
      catch { throw new Error(`Issue #${issue}: local branch ${branch} is missing. Restore the task branch locally and retry.`); }
      try { git(['merge-base', '--is-ancestor', ref, commit]); }
      catch { throw new Error(`Issue #${issue}: ${branch} is not an ancestor of ${commit}. Merge the branch tip and retry.`); }
    }
  }
  const id = `${version}-${commit.slice(0, 8)}`;
  const target = join(releases, id);
  if (existsSync(target)) throw new Error(`Release already exists: ${id}`);
  if (build) for (const script of ['lint', 'build', 'test']) execFileSync('npm', ['run', script], { cwd: source, stdio: 'inherit' });
  const rust = existsSync(join(repository, 'Cargo.toml'));
  if (build && rust) {
    // Publication copies from this directory, so a caller's CARGO_TARGET_DIR must not redirect the build.
    const cargoEnv = { ...process.env, CARGO_TARGET_DIR: join(repository, 'target') };
    for (const args of [['fmt', '--check'], ['clippy', '--all-targets', '--', '-D', 'warnings'], ['test'], ['build', '--release', '--locked']]) execFileSync('cargo', args, { cwd: repository, stdio: 'inherit', env: cargoEnv });
    execFileSync(process.execPath, [join(repository, 'scripts/resource-gate.mjs'), join(repository, 'target/release/minarbot-server'), join(repository, 'scripts/budgets.json'), join(repository, 'target/release/minarbot-launcher')], { cwd: repository, stdio: 'inherit', env: { ...process.env, TMPDIR: '/dev/shm' } });
    const gate = repository === root
      ? ['systemd-run', '--user', '--scope', '-q', '-p', 'MemoryMax=4G', '-p', 'MemorySwapMax=0', 'env', 'NODE_OPTIONS=--max-old-space-size=2048', 'TMPDIR=/dev/shm', process.execPath]
      : [process.execPath];
    execFileSync(gate[0], [...gate.slice(1), join(repository, 'scripts/production-env-gate.mjs'), join(repository, 'target/release/minarbot-server'), join(repository, 'target/release/minarbot-launcher')], { cwd: repository, stdio: 'inherit' });
  }
  const computerImage = buildImage?.(commit);
  mkdirSync(releases, { recursive: true, mode: 0o700 });
  const stage = mkdtempSync(join(releases, '.staging-'));
  try {
    // Runtime source only. Never include workspace data, .env, test fixtures or development bundles.
    stageRuntime(source, stage);
    if (rust && existsSync(join(repository, 'target/release/minarbot-server')) && existsSync(join(repository, 'target/release/minarbot-launcher'))) {
      mkdirSync(join(stage, 'bin'), { recursive: true, mode: 0o700 }); cpSync(join(repository, 'target/release/minarbot-server'), join(stage, 'bin/minarbot-server'));
      cpSync(join(repository, 'target/release/minarbot-launcher'), join(stage, 'bin/minarbot-launcher'));
      for (const name of ['minarbot-server', 'minarbot-launcher']) assertNativeDependencies(join(stage, 'bin', name));
      cpSync(join(repository, 'crates/minarbot-launcher/provenance.json'), join(stage, 'bin/launcher-provenance.json'));
      cpSync(join(repository, 'crates/minarbot-launcher/NOTICE'), join(stage, 'bin/launcher.NOTICE'));
      cpSync(join(repository, 'crates/minarbot-server/provenance.json'), join(stage, 'bin/provenance.json'));
      for (const name of ['LICENSE', 'agent-delegation.LICENSE']) cpSync(join(repository, 'app', name), join(stage, 'bin', name));
    }
    mkdirSync(join(stage, 'machine'), { mode: 0o700 });
    for (const name of ['install-machine.sh', 'uninstall-machine.sh']) cpSync(join(root, 'scripts', name), join(stage, 'machine', name));
    const info = parseRelease({ id, version, commit, issues, created: new Date().toISOString(), node: 'rust', notes: notes ?? git(['log', '-5', '--pretty=%s']), files: fileManifest(stage), ...(computerImage && { computerImage }) });
    durableWrite(join(stage, 'release.json'), JSON.stringify(info, null, 2) + '\n', 0o644);
    renameSync(stage, target); syncDirectory(releases);
    mkdirSync(join(releases, 'feed'), { recursive: true, mode: 0o700 });
    // Immutable directory first; replace feed metadata only after complete runtime publication.
    durableWrite(join(releases, 'feed/latest.json'), JSON.stringify(info, null, 2) + '\n', 0o644);
    return info;
  } finally { rmSync(stage, { recursive: true, force: true }); }
}
// Copies the runtime selection a release ships; tests stage the same selection to run the import guard.
export function stageRuntime(source, stage) {
  for (const name of ['bin', 'dist', 'package.json', 'LICENSE', 'novnc.LICENSE']) {
    if (existsSync(join(source, name))) cpSync(join(source, name), join(stage, name), { recursive: true });
  }
  for (const name of ['NOTICE', 'provenance.json']) {
    const file = join(source, 't3', name);
    if (!existsSync(file)) continue;
    mkdirSync(join(stage, 't3'), { recursive: true });
    cpSync(file, join(stage, 't3', name));
  }
  const command = join(source, 'scripts', 'minarbot-command.sh');
  if (existsSync(command)) {
    mkdirSync(join(stage, 'scripts'), { recursive: true });
    cpSync(command, join(stage, 'scripts', 'minarbot-command.sh'));
  }
  const worker = join(source, 'shared-data-worker.py');
  if (existsSync(worker)) {
    mkdirSync(join(stage, 'app'), { recursive: true });
    cpSync(worker, join(stage, 'app', 'shared-data-worker.py'));
  }
}
async function releaseCommand() {
  const { values } = parseArgs({ options: {
    releases: { type: 'string' }, notes: { type: 'string' }, issues: { type: 'string' }, desktop: { type: 'boolean' },
    'allow-dirty': { type: 'boolean' }, 'no-github': { type: 'boolean' }, 'github-only': { type: 'string' }, 'signing-key': { type: 'string' },
    'no-image': { type: 'boolean' },
  } });
  const releases = resolve(values.releases ?? join(homedir(), '.local/share/minarbot/releases'));
  const keyPath = values['signing-key'] ?? process.env.MINARBOT_RELEASE_SIGNING_KEY
    ?? join(homedir(), '.config/minarbot-release/signing-ed25519.pem');
  const retryId = values['github-only'], allowDirty = values['allow-dirty'], { notes, desktop } = values;
  if (retryId !== undefined) {
    if ([allowDirty, values['no-github'], values['no-image'], values.issues, notes, desktop].some(value => value !== undefined)) {
      throw new Error('--github-only accepts only --releases and --signing-key.');
    }
    checkCleanTree();
    await uploadPublishedRelease(releases, retryId, keyPath);
    return;
  }

  const githubEnabled = !values['no-github'] && !allowDirty;
  if (!allowDirty) checkCleanTree();
  else if (!values['no-github']) console.warn('--allow-dirty publishes locally only.');
  if (githubEnabled && !notes?.trim()) throw new Error('Pass --notes with user-facing release notes for GitHub, or use --no-github.');
  if (values.issues !== undefined && !/^\d+(,\d+)*$/.test(values.issues)) throw new Error('Invalid issues. Use --issues 30,31 with positive issue numbers.');
  let computerImage;
  if (githubEnabled && !values['no-image']) {
    const { MINARBOT_GHCR_USER: user, MINARBOT_GHCR_TOKEN: token } = process.env;
    if (!user || !token) throw new Error('Set MINARBOT_GHCR_USER and MINARBOT_GHCR_TOKEN to publish the Computer image, or pass --no-image.');
    delete process.env.MINARBOT_GHCR_TOKEN;
    computerImage = commit => {
      checkCleanTree();
      if (execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim() !== commit) throw new Error('Worktree HEAD changed during the release build.');
      return publishComputerImage(commit, user, token);
    };
  }
  const info = buildRelease({ releases, allowDirty, notes, issues: values.issues?.split(',').map(Number), computerImage });
  console.log(`Published ${info.id} locally`);

  const directory = publicationDirectory(releases, info.id);
  let desktopHashes;
  if (desktop) {
    mkdirSync(directory, { recursive: true, mode: 0o700 });
    const stage = mkdtempSync(join(directory, '.desktop-'));
    try {
      execFileSync(process.execPath, [join(root, 'desktop/scripts/build.mjs'), info.version, stage], { cwd: root, stdio: 'inherit' });
      const files = desktopAssets(stage, info.version);
      desktopHashes = await Promise.all(files.map(sha256));
      for (const file of files) renameSync(file, join(directory, basename(file)));
      syncDirectory(directory);
    } finally { rmSync(stage, { recursive: true, force: true }); }
  }
  if (!allowDirty && notes?.trim()) {
    checkCleanTree();
    if (execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim() !== info.commit) throw new Error('Worktree HEAD changed during the release build.');
    mkdirSync(directory, { recursive: true, mode: 0o700 });
    durableWrite(join(directory, 'ready.json'), JSON.stringify({
      release: await sha256(join(releases, info.id, 'release.json')), desktop: desktopHashes,
    }));
  }
  if (githubEnabled) await uploadPublishedRelease(releases, info.id, keyPath);
}

async function uploadPublishedRelease(releases, id, keyPath) {
  try { await publishGithub(releases, id, keyPath); }
  catch (error) {
    throw new Error(`GitHub publication failed. Local release ${id} is unchanged. ${error.message}\nAfter resolving the error, retry with --github-only ${id} and the same --releases directory.`);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try { await releaseCommand(); }
  catch (error) { console.error(`Error: ${error.message}`); process.exitCode = 1; }
}

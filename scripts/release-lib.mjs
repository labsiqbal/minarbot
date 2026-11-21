// Native release integrity and durable publication. Shared by server, builder and stable launcher.
import { createHash, randomUUID } from 'node:crypto';
import { closeSync, constants, fsyncSync, lstatSync, mkdirSync, openSync, readFileSync, readdirSync, realpathSync, renameSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';

export const releaseIdPattern = /^\d+\.\d+\.\d+(?:-[A-Za-z0-9.-]+)?-[a-f0-9]{8,40}$/;
export function durableWrite(path, bytes, mode = 0o600) {
  const temp = `${path}.tmp-${randomUUID()}`;
  let fd;
  try {
    fd = openSync(temp, constants.O_CREAT | constants.O_EXCL | constants.O_WRONLY | constants.O_NOFOLLOW, mode);
    writeFileSync(fd, bytes); fsyncSync(fd); closeSync(fd); fd = undefined;
    renameSync(temp, path); syncDirectory(dirname(path));
  } finally { if (fd !== undefined) closeSync(fd); rmSync(temp, { force: true }); }
}
export function syncDirectory(path) { const fd = openSync(path, constants.O_RDONLY); try { fsyncSync(fd); } finally { closeSync(fd); } }
export function selectRelease(releases, name, id) {
  if (!['current', 'previous', 'trial'].includes(name) || !releaseIdPattern.test(id)) throw new Error('Invalid release selection.');
  const path = join(releases, name), temp = `${path}.tmp-${randomUUID()}`;
  try { symlinkSync(id, temp); renameSync(temp, path); syncDirectory(releases); } finally { rmSync(temp, { force: true }); }
}
export function selectedRelease(releases, name = 'current') {
  const path = join(releases, name);
  if (!lstatSync(path, { throwIfNoEntry: false })) return undefined;
  if (!lstatSync(path).isSymbolicLink()) throw new Error(`${name} must be a release symlink.`);
  const target = realpathSync(path), id = target.slice(target.lastIndexOf('/') + 1);
  if (target !== join(realpathSync(releases), id) || !releaseIdPattern.test(id)) throw new Error('Release selection escapes releases directory.');
  return id;
}
export function fileManifest(directory) {
  const files = {};
  function visit(path, prefix = '') {
    for (const name of readdirSync(path).sort()) {
      const full = join(path, name), relative = prefix + name, stat = lstatSync(full);
      if (stat.isDirectory()) visit(full, relative + '/');
      else if (relative === 'release.json' && stat.isFile()) continue;
      else if (stat.isFile()) files[relative] = createHash('sha256').update(readFileSync(full)).digest('hex');
      else throw new Error(`Release contains unsupported file: ${relative}`);
    }
  }
  visit(directory); return files;
}
export function parseRelease(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value) || !releaseIdPattern.test(value.id) || !/^\d+\.\d+\.\d+(?:-[A-Za-z0-9.-]+)?$/.test(value.version) || !/^[a-f0-9]{40}$/.test(value.commit) || !value.id.startsWith(value.version + '-') || !value.commit.startsWith(value.id.slice(value.version.length + 1)) || typeof value.created !== 'string' || !Number.isFinite(Date.parse(value.created)) || typeof value.node !== 'string' || typeof value.notes !== 'string' || value.notes.length > 16000 || !value.files || typeof value.files !== 'object' || Array.isArray(value.files)) throw new Error('Invalid release metadata.');
  for (const [path, hash] of Object.entries(value.files)) if (!path || path.startsWith('/') || path.split('/').some(p => !p || p === '.' || p === '..') || /[\\\x00-\x1f\x7f]/.test(path) || !/^[a-f0-9]{64}$/.test(hash)) throw new Error('Invalid release file manifest.');
  if (value.issues !== undefined && (!Array.isArray(value.issues) || value.issues.some(issue => !Number.isSafeInteger(issue) || issue <= 0) || new Set(value.issues).size !== value.issues.length)) throw new Error('Invalid release issues.');
  if (value.computerImage !== undefined && (typeof value.computerImage !== 'string' || !/^sha256:[a-f0-9]{64}$/.test(value.computerImage))) throw new Error('Invalid Computer image digest.');
  return value;
}
export function verifyRelease(releases, id) {
  if (!releaseIdPattern.test(id)) throw new Error('Invalid release ID.');
  const path = join(resolve(releases), id);
  if (lstatSync(path).isSymbolicLink() || realpathSync(path) !== join(realpathSync(releases), id)) throw new Error('Invalid release directory.');
  const info = parseRelease(JSON.parse(readFileSync(join(path, 'release.json'), 'utf8')));
  if (info.id !== id || JSON.stringify(Object.entries(fileManifest(path)).sort()) !== JSON.stringify(Object.entries(info.files).sort())) throw new Error('Release SHA-256 manifest mismatch.');
  if (JSON.parse(readFileSync(join(path, 'package.json'), 'utf8')).version !== info.version) throw new Error('Release package version mismatch.');
  return info;
}
export function latestRelease(releases) {
  try { return parseRelease(JSON.parse(readFileSync(join(releases, 'feed/latest.json'), 'utf8'))); }
  catch (error) { if (error.code === 'ENOENT') return undefined; throw error; }
}
export function newerRelease(candidate, current) {
  const parts = version => version.split('-')[0].split('.').map(Number), a = parts(candidate), b = parts(current);
  for (let i = 0; i < 3; i++) if (a[i] !== b[i]) return a[i] > b[i];
  return candidate !== current && current.includes('-') && !candidate.includes('-');
}

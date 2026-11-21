import { createHash, createPublicKey, verify } from 'node:crypto';
import { createWriteStream } from 'node:fs';
import { mkdtemp, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { Readable, Transform } from 'node:stream';
import { pipeline } from 'node:stream/promises';

const releases = 'https://github.com/labsiqbal/minarbot/releases';
const hosts = ['github.com', 'objects.githubusercontent.com', 'release-assets.githubusercontent.com'];
const key = createPublicKey({
  key: Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), Buffer.from('kSIv+agT4RsABust/LmvPVjfWDxZxM0YKL03gts/UEM=', 'base64')]),
  format: 'der', type: 'spki',
});
const versionPattern = /^[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?$/;
const limit = 1024 ** 3;

async function get(url, timeout) {
  const signal = AbortSignal.timeout(timeout);
  for (let hop = 0; hop < 4; hop++) {
    const parsed = new URL(url);
    if (parsed.protocol !== 'https:' || parsed.port || parsed.username || parsed.password || !hosts.includes(parsed.hostname)) throw new Error('Invalid release URL.');
    const response = await fetch(parsed, { signal, redirect: 'manual' });
    if (response.ok && response.body) return response.body;
    await response.body?.cancel();
    const redirect = response.headers.get('location');
    if (response.status < 300 || response.status >= 400 || !redirect) break;
    url = new URL(redirect, parsed).href;
  }
  throw new Error('Could not download the release from GitHub.');
}

async function smallAsset(name, max) {
  const chunks = [];
  let length = 0;
  for await (const chunk of await get(`${releases}/latest/download/${name}`, 8000)) {
    length += chunk.length;
    if (length > max) throw new Error('Invalid release metadata size.');
    chunks.push(chunk);
  }
  return Buffer.concat(chunks);
}

export function macUpdateAsset(bytes, signature, current, arch, publicKey = key) {
  if (signature.length !== 88 || !verify(null, bytes, publicKey, Buffer.from(signature.toString(), 'base64'))) {
    throw new Error('Invalid release signature.');
  }
  const { version, assets } = JSON.parse(bytes);
  if (typeof version !== 'string' || !versionPattern.test(version) || !['arm64', 'x64'].includes(arch)) throw new Error('Invalid release metadata.');
  const order = version.split('-')[0].localeCompare(current.split('-')[0], 'en', { numeric: true });
  if (order < 0 || (order === 0 && (!current.includes('-') || version.includes('-')))) return null;
  const name = `Minarbot-${version}-macos-${arch}.dmg`;
  const asset = assets?.[name];
  if (!asset || !Number.isSafeInteger(asset.size) || asset.size < 1 || asset.size > limit || !/^[a-f0-9]{64}$/.test(asset.sha256)) {
    throw new Error('Missing macOS update.');
  }
  return { name, version, size: asset.size, sha256: asset.sha256 };
}

async function download(asset, downloads, open) {
  const directory = await mkdtemp(join(downloads, 'Minarbot-'));
  const file = join(directory, asset.name);
  const hash = createHash('sha256');
  let size = 0;
  try {
    const stream = await get(`${releases}/download/v${asset.version}/${asset.name}`, 300000);
    const validate = new Transform({ transform(chunk, _, done) {
      size += chunk.length;
      if (size > asset.size) return done(new Error('Update exceeded expected size.'));
      hash.update(chunk);
      done(null, chunk);
    } });
    await pipeline(Readable.fromWeb(stream), validate, createWriteStream(file, { flags: 'wx', mode: 0o600 }));
    if (size !== asset.size || hash.digest('hex') !== asset.sha256) throw new Error('Update integrity check failed.');
    const message = await open(file);
    if (message) throw new Error(message);
  } catch (error) {
    await rm(directory, { recursive: true, force: true });
    throw error;
  }
}

export function configureMacUpdates({ version, arch, downloads, open, prompt }) {
  let busy = false;
  return { stop() {}, async check() {
    if (busy) return;
    busy = true;
    try {
      const [bytes, signature] = await Promise.all([smallAsset('latest.json', 128 * 1024), smallAsset('latest.json.sig', 88)]);
      const asset = macUpdateAsset(bytes, signature, version, arch);
      if (!asset) {
        await prompt({ title: 'Updates', message: 'Minarbot is up to date.', buttons: ['OK'] });
      } else if (await prompt({ title: 'Update available', message: `Download Minarbot ${asset.version}?`,
        detail: 'Quit Minarbot, then replace it in Applications from the disk image.',
        buttons: ['Download and open', 'Later'], defaultId: 1, cancelId: 1 }) === 0) await download(asset, downloads, open);
    } catch {
      await prompt({ title: 'Update failed', message: 'Could not verify or open the macOS update. Try again later.', buttons: ['OK'] });
    } finally { busy = false; }
  } };
}

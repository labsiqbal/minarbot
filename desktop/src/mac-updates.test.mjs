import test from 'node:test';
import assert from 'node:assert/strict';
import { generateKeyPairSync, sign } from 'node:crypto';
import { macUpdateAsset } from './mac-updates.mjs';

test('macOS updates select the same-architecture DMG from a signed newer release', () => {
  const { privateKey, publicKey } = generateKeyPairSync('ed25519');
  const version = '1.10.0';
  const name = `Minarbot-${version}-macos-arm64.dmg`, asset = { size: 1024, sha256: 'a'.repeat(64) };
  const bytes = Buffer.from(JSON.stringify({ version, assets: { [name]: asset } }));
  const signature = Buffer.from(sign(null, bytes, privateKey).toString('base64'));
  assert.deepEqual(macUpdateAsset(bytes, signature, '1.9.0', 'arm64', publicKey), { name, version, ...asset });
  assert.equal(macUpdateAsset(bytes, signature, version, 'arm64', publicKey), null);
  assert.throws(() => macUpdateAsset(bytes, signature, '1.9.0', 'x64', publicKey), /Missing macOS update/);
  assert.throws(() => macUpdateAsset(Buffer.concat([bytes, Buffer.from(' ')]), signature, '1.9.0', 'arm64', publicKey), /Invalid release signature/);
});

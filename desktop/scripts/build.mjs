// Adapted from T3 scripts/build-desktop-artifact.ts. See ../provenance.json.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { build } from 'electron-builder';
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
process.env.ELECTRON_CACHE ??= path.join(root, '.cache/electron');
process.env.ELECTRON_BUILDER_CACHE ??= path.join(root, '.cache/builder');
process.env.TMPDIR ??= path.join(root, '.cache/tmp');
fs.mkdirSync(process.env.TMPDIR, { recursive: true });
const [version, output] = process.argv.slice(2);
const archive = process.env.MINARBOT_LINUX_ARCHIVE;
if (process.platform === 'win32' && !archive) throw new Error('Set MINARBOT_LINUX_ARCHIVE to the Linux x86_64 runtime archive.');
const resources = archive ? [
  { from: path.resolve(archive), to: 'minarbot-linux.tar.gz' },
  { from: path.resolve(root, '../scripts/install.sh'), to: 'install.sh' },
  { from: path.resolve(root, '../scripts/install.ps1'), to: 'install.ps1' },
] : [];
await build({
  projectDir: root,
  publish: 'never',
  config: {
    ...(version && { extraMetadata: { version } }),
    ...(output && { directories: { output } }),
    ...(resources.length && { extraResources: resources }),
    ...(process.platform === 'darwin' && { mac: {
      artifactName: 'Minarbot-${version}-macos-${arch}.${ext}',
      ...(!process.env.CSC_LINK && { identity: '-', hardenedRuntime: false }),
    } }),
    ...(process.platform === 'win32' && { win: { artifactName: 'Minarbot-${version}-windows-${arch}.${ext}' } }),
  },
});

import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { gzipSync } from 'node:zlib';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const app = join(root, 'app');
const web = join(root, 'crates/web');
const target = join(web, 'target');
const dist = process.argv[2] ? resolve(process.argv[2]) : join(app, 'dist');
const version = '0.2.130';
const namespace = '/_leptos/';
const slot = process.env.MINARBOT_CARGO_SLOT;
const require = createRequire(join(app, 'package.json'));
const { build } = require('vite');
const { createElement } = require('react');
const { renderToStaticMarkup } = require('react-dom/server');
const { CircleAlert, Search, X } = require('lucide-react');
const { colorThemes, themeStyles } = await import('../app/src/ui/themes.ts');
const { settingsGroups } = await import('../app/src/settings-groups.ts');
const sha = bytes => createHash('sha256').update(bytes).digest('hex');

function bindgen() {
  const host = ({ 'linux-x64': 'x86_64-unknown-linux-musl', 'linux-arm64': 'aarch64-unknown-linux-musl', 'darwin-x64': 'x86_64-apple-darwin', 'darwin-arm64': 'aarch64-apple-darwin' })[process.platform + '-' + process.arch];
  if (!host) throw new Error('Unsupported host for pinned wasm-bindgen CLI.');
  const checksums = {
    'x86_64-unknown-linux-musl': '9687f3bb551b9f66f37ed32a40d7913a00f7962db653054e56857bc306c3227f',
    'aarch64-unknown-linux-musl': '99ffc93ce80081b91632e2b539b782821a8f3439ab0fddc645287462b2f55d3d',
    'x86_64-apple-darwin': 'eb209c115b7cb95832423c1aa3f4d06d6c53f63ef74fc4b6fb79ef0b2eea5f10',
    'aarch64-apple-darwin': 'b90ddb9357f584b1af897d58ad77e4cce2b48fc17e2a967bc249a87f72809444',
  };
  const name = 'wasm-bindgen-' + version + '-' + host;
  const bin = join(target, name, 'wasm-bindgen');
  if (!existsSync(bin)) {
    const temp = mkdtempSync(join(target, 'bindgen-cli-'));
    try {
      const archive = join(temp, name + '.tar.gz');
      execFileSync('curl', ['--fail', '--silent', '--show-error', '--location', '--retry', '2', '--max-time', '90', '--output', archive,
        'https://github.com/wasm-bindgen/wasm-bindgen/releases/download/' + version + '/' + name + '.tar.gz'], { stdio: 'inherit' });
      if (sha(readFileSync(archive)) !== checksums[host]) throw new Error('wasm-bindgen download checksum mismatch.');
      execFileSync('tar', ['-xzf', archive, '-C', temp], { stdio: 'inherit' });
      renameSync(join(temp, name), join(target, name));
    } finally { if (existsSync(temp)) rmSync(temp, { recursive: true }); }
  }
  if (execFileSync(bin, ['--version'], { encoding: 'utf8' }).trim() !== 'wasm-bindgen ' + version) throw new Error('Unexpected wasm-bindgen CLI version.');
  return bin;
}
const sizes = {};
function asset(stage, file, bytes) {
  const path = join(stage, file);
  writeFileSync(path, bytes);
  const raw = Buffer.byteLength(bytes);
  sizes[file] = { raw };
  if (!file.endsWith('.woff2')) {
    const gzip = gzipSync(bytes, { level: 9 });
    writeFileSync(path + '.gz', gzip);
    sizes[file].gzip = gzip.length;
  }
  return namespace + file;
}
function hashed(stage, name, extension, bytes) {
  return asset(stage, 'assets/' + name + '.' + sha(bytes).slice(0, 16) + '.' + extension, bytes);
}

mkdirSync(target, { recursive: true });
const cli = bindgen();
const began = performance.now();
const cargoArgs = ['build', '--locked', '--release', '--target', 'wasm32-unknown-unknown'];
execFileSync(slot ?? 'cargo', slot ? ['cargo', ...cargoArgs] : cargoArgs,
  { cwd: web, stdio: 'inherit', env: { ...process.env, CARGO_TARGET_DIR: target } });
const cargoFinished = performance.now();
mkdirSync(dist, { recursive: true });
const temp = mkdtempSync(join(target, 'bindgen-'));
const stage = mkdtempSync(join(dist, '.leptos-'));
try {
  mkdirSync(join(stage, 'assets'));
  execFileSync(cli, ['--target', 'web', '--out-dir', temp, '--out-name', 'appearance', '--no-typescript', join(target, 'wasm32-unknown-unknown/release/minarbot_web.wasm')], { stdio: 'inherit' });
  const wasm = hashed(stage, 'appearance', 'wasm', readFileSync(join(temp, 'appearance_bg.wasm')));
  const glue = join(temp, 'appearance.js');
  writeFileSync(glue, readFileSync(glue, 'utf8').replaceAll('appearance_bg.wasm', wasm));
  writeFileSync(join(temp, 'start.js'), readFileSync(join(web, 'start.js'), 'utf8').replaceAll('__WASM_URL__', wasm));
  const result = await build({
    root: app,
    base: namespace,
    configFile: join(app, 'vite.config.ts'),
    publicDir: false,
    build: {
      write: false,
      reportCompressedSize: false,
      rollupOptions: {
        input: { start: join(temp, 'start.js'), appearance: join(web, 'appearance.css') },
        output: {
          hashCharacters: 'hex',
          entryFileNames: 'assets/[name].[hash:16].js',
          chunkFileNames: 'assets/[name].[hash:16].js',
          assetFileNames: 'assets/[name].[hash:16][extname]',
        },
      },
    },
  });
  let styles, start;
  for (const file of result.output) {
    const url = asset(stage, file.fileName, file.type === 'asset' ? file.source : file.code);
    if (file.fileName.endsWith('.css')) styles = url;
    if (file.type === 'chunk' && file.isEntry && file.name === 'start') start = url;
  }
  if (!styles || !start) throw new Error('Appearance CSS or bootstrap is missing.');
  const themes = hashed(stage, 'themes', 'css', themeStyles());
  const icon = hashed(stage, 'minarbot', 'svg', readFileSync(join(app, 'src/favicon.svg')));
  const svg = (icon, size = 16) => renderToStaticMarkup(createElement(icon, { size, 'aria-hidden': true }));
  const config = JSON.stringify({
    colors: colorThemes,
    groups: settingsGroups.map(group => ({
      label: group.label,
      items: group.items.map(({ id, label, icon }) => ({ id, label, svg: svg(icon) })),
    })),
    searchIcon: svg(Search, 14),
    closeIcon: svg(X),
    errorIcon: svg(CircleAlert),
  }).replaceAll('<', '\\u003c');
  let html = readFileSync(join(web, 'appearance.html'), 'utf8');
  for (const [key, value] of Object.entries({ ICON_URL: icon, STYLES_URL: styles, THEMES_URL: themes, START_URL: start, CONFIG: config })) {
    html = html.replace('__' + key + '__', () => value);
  }
  writeFileSync(join(stage, 'appearance.html'), html);
  for (const name of ['NOTICE', 'NOTICE.hermes', 'NOTICE.t3', 'provenance.json']) copyFileSync(join(app, 'src/ui', name), join(stage, 'ui.' + name));
  copyFileSync(join(app, 'node_modules/lucide-react/LICENSE'), join(stage, 'lucide.LICENSE'));
  copyFileSync(join(app, 'node_modules/@fontsource-variable/inter/LICENSE'), join(stage, 'inter.LICENSE'));
  for (const name of ['NOTICE', 'leptos.LICENSE']) copyFileSync(join(web, name), join(stage, name));
  for (const name of ['LICENSE-MIT', 'LICENSE-APACHE']) copyFileSync(join(dirname(cli), name), join(stage, 'wasm-bindgen.' + name));
  const output = join(dist, '_leptos');
  if (existsSync(output)) rmSync(output, { recursive: true });
  renameSync(stage, output);
  console.log(JSON.stringify({ cargoWallSeconds: (cargoFinished - began) / 1000, packageSeconds: (performance.now() - cargoFinished) / 1000, sizes }, null, 2));
} finally {
  if (existsSync(temp)) rmSync(temp, { recursive: true });
  if (existsSync(stage)) rmSync(stage, { recursive: true });
}

import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createWriteStream } from 'node:fs';
import { mkdir, mkdtemp, readdir, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import sharp from 'sharp';

// The prior review's settled conversation pairs peaked at 61 changed pixels and 3 over delta 16.
const maxChanged = 100, maxOver16 = 24;
const cwd = resolve(import.meta.dirname, '../..');
const root = process.argv[2] ? resolve(process.argv[2]) : await mkdtemp(join(tmpdir(), 'minarbot-parity-'));
if (process.argv[2]) {
  await mkdir(root, { recursive: true });
  assert.equal((await readdir(root)).length, 0, `Parity output must be empty: ${root}`);
}
const reports = join(root, 'pairs');
await mkdir(reports, { recursive: true });
console.log(`Parity artifacts: ${root}`);

const groups = [
  ['appearance', 20, 'appearance', 'MINARBOT_APPEARANCE_SCREENSHOTS'],
  ['automation', 40, 'automation-parity'],
  ['calendar', 12, 'automation-calendar', null, 'MINARBOT_SMOKE_UI'],
  ['connections', 36, 'settings-connections', 'MINARBOT_CONNECTION_SCREENSHOTS'],
  ['controls', 58, 'conversation-controls-parity'],
  ['conversation', 4, 'leptos-conversation', 'MINARBOT_CONVERSATION_SCREENSHOTS'],
  ['files', 52, 'conversation-files-parity'],
  ['guide', 16, 'guide', 'MINARBOT_GUIDE_SCREENSHOTS', 'MINARBOT_SMOKE_UI'],
  ['hub', 26, 'hub-parity', 'MINARBOT_HUB_SCREENSHOTS'],
  ['memory', 32, 'memory'],
  ['profiles', 64, 'agent-profile-parity'],
  ['projects', 68, 'project'],
  ['rich', 32, 'conversation-parity'],
  ['scoped', 52, 'conversation-automation-parity'],
  ['shell', 36, 'workspace-prompts'],
  ['system', 32, 'settings-system', 'MINARBOT_SYSTEM_SCREENSHOTS'],
];

async function smoke(script, output, env, log) {
  const stream = createWriteStream(log);
  const code = await new Promise((resolve, reject) => {
    const child = spawn(process.execPath, ['--experimental-strip-types', join(import.meta.dirname, `${script}-smoke.mjs`), output], {
      cwd, env, stdio: ['ignore', 'pipe', 'pipe'],
    });
    child.stdout.pipe(stream, { end: false });
    child.stderr.pipe(stream, { end: false });
    child.once('error', reject);
    child.once('close', (code, signal) => resolve(code ?? signal));
  }).finally(() => new Promise(resolve => stream.end(resolve)));
  return code;
}

function label(filename, client) {
  const marker = new RegExp(`(^|-)${client}(?=-|\\.png$)`);
  if (marker.test(filename)) return filename.replace(marker, '').replace(/^-/, '');
  return null;
}

async function compare(left, right, output) {
  const [react, leptos] = await Promise.all([left, right].map(path =>
    sharp(path).toColourspace('srgb').removeAlpha().raw().toBuffer({ resolveWithObject: true })));
  const { width, height } = react.info;
  const sameSize = width === leptos.info.width && height === leptos.info.height;
  await sharp({ create: { width: width + 8 + leptos.info.width, height: Math.max(height, leptos.info.height), channels: 3, background: '#181c24' } })
    .composite([{ input: left, left: 0, top: 0 }, { input: right, left: width + 8, top: 0 }]).png().toFile(output);
  if (!sameSize) return { changed: null, over16: null, box: '', result: 'dimensions' };
  let changed = 0, over16 = 0, minX = width, minY = height, maxX = 0, maxY = 0;
  for (let pixel = 0; pixel < width * height; pixel++) {
    const i = 3 * pixel;
    const delta = Math.max(Math.abs(react.data[i] - leptos.data[i]), Math.abs(react.data[i + 1] - leptos.data[i + 1]), Math.abs(react.data[i + 2] - leptos.data[i + 2]));
    if (!delta) continue;
    changed++;
    if (delta > 16) over16++;
    const x = pixel % width, y = Math.floor(pixel / width);
    minX = Math.min(minX, x); minY = Math.min(minY, y);
    maxX = Math.max(maxX, x + 1); maxY = Math.max(maxY, y + 1);
  }
  return { changed, over16, box: changed ? `${minX},${minY},${maxX},${maxY}` : '',
    result: changed > maxChanged || over16 > maxOver16 ? 'pixels' : 'pass' };
}

const only = process.env.PARITY_GROUPS?.split(',').map(name => name.trim()).filter(Boolean);
if (only) {
  const unknown = only.filter(name => !groups.some(([group]) => group === name));
  assert.equal(unknown.length, 0, `Unknown parity groups: ${unknown.join(', ')}`);
}
const selected = only ? groups.filter(([group]) => only.includes(group)) : groups;

const env = { ...process.env, MINARBOT_RUST_BINARY: resolve(process.env.MINARBOT_RUST_BINARY ?? join(cwd, '../target/debug/minarbot-server')) };
env.MINARBOT_HOSTED_HUB_BINARY ??= env.MINARBOT_RUST_BINARY;
for (const key of ['MINARBOT_SMOKE_UI', 'MINARBOT_SYSTEM_CLIENT', 'MINARBOT_HUB_SMOKE_CLIENT', 'MINARBOT_PROMPTS_CLIENT']) delete env[key];
for (const key of ['MINARBOT_PROJECT_BENCH', 'MINARBOT_FILES_BENCH', 'MINARBOT_CONVERSATION_NAV_BENCH']) env[key] ??= '0';
const rows = [], summary = [];
for (const [group, minimum, script, screenshotEnv, clientEnv] of selected) {
  const dir = join(root, group);
  await mkdir(dir);
  const clients = clientEnv ? ['react', 'leptos'] : ['both'];
  const checks = {};
  const images = {};
  for (const client of clients) {
    const output = clientEnv && !screenshotEnv ? join(dir, client) : dir;
    await mkdir(output, { recursive: true });
    const settings = { ...env, ...(clientEnv ? { [clientEnv]: client } : {}), ...(screenshotEnv ? { [screenshotEnv]: output } : {}) };
    checks[client] = await smoke(script, output, settings, join(dir, `${client}.log`));
  }
  for (const client of ['react', 'leptos']) {
    const separate = clientEnv && !screenshotEnv;
    const output = separate ? join(dir, client) : dir;
    images[client] = new Map();
    for (const file of (await readdir(output)).sort()) {
      if (!file.endsWith('.png') || /^(side-by-side-|failure-)|-(failure|diagnostic)\.png$/.test(file)) continue;
      const name = separate ? file : label(file, client);
      if (!name) continue;
      assert(!images[client].has(name), `Duplicate screenshot: ${group}/${client}/${name}`);
      images[client].set(name, join(output, file));
    }
  }
  const groupRows = [];
  await mkdir(join(reports, group));
  for (const name of new Set([...images.react.keys(), ...images.leptos.keys()])) {
    const react = images.react.get(name), leptos = images.leptos.get(name);
    const result = react && leptos ? await compare(react, leptos, join(reports, group, name))
      : { changed: null, over16: null, box: '', result: `missing ${react ? 'Leptos' : 'React'}` };
    groupRows.push({ pair: `${group}/${name}`, ...result });
  }
  rows.push(...groupRows);
  const pairs = groupRows.filter(row => !row.result.startsWith('missing')).length;
  const failures = groupRows.filter(row => row.result !== 'pass').length;
  const stats = { group, pairs, minimum, failures, maxChanged: Math.max(0, ...groupRows.map(row => row.changed ?? 0)),
    maxOver16: Math.max(0, ...groupRows.map(row => row.over16 ?? 0)), checks };
  summary.push(stats);
  console.log(JSON.stringify(stats));
}
const header = 'pair\tchanged\tover16\tbox\tresult';
const tsv = entries => [header, ...entries.map(row => [row.pair, row.changed ?? '', row.over16 ?? '', row.box, row.result].join('\t')), ''].join('\n');
await writeFile(join(reports, 'all.tsv'), tsv(rows));
await writeFile(join(reports, 'nonidentical.tsv'), tsv(rows.filter(row => row.changed || row.result !== 'pass')));
await writeFile(join(reports, 'failures.tsv'), tsv(rows.filter(row => row.result !== 'pass')));
await writeFile(join(reports, 'summary.json'), JSON.stringify(summary, null, 2) + '\n');
if (summary.some(group => group.failures || group.pairs < group.minimum || Object.values(group.checks).some(code => code !== 0))) process.exitCode = 1;

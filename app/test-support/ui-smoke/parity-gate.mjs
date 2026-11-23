import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createWriteStream } from 'node:fs';
import { mkdir, mkdtemp, readdir, rename, stat, unlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import sharp from 'sharp';

const maxChanged = 100, maxOver16 = 24;
const baselines = join(import.meta.dirname, 'baselines');
const failed = row => row.result !== 'pass';
const png = file => file.endsWith('.png') && !/^(side-by-side-|pair-)|-(failure|diagnostic)\.png$/.test(file);
const groups = [
  ['appearance', 20, 'appearance', 'MINARBOT_APPEARANCE_SCREENSHOTS'],
  ['automation', 40, 'automation-parity'],
  ['calendar', 12, 'automation-calendar'],
  ['connections', 32, 'settings-connections', 'MINARBOT_CONNECTION_SCREENSHOTS'],
  ['controls', 58, 'conversation-controls-parity'],
  ['conversation', 4, 'leptos-conversation', 'MINARBOT_CONVERSATION_SCREENSHOTS'],
  ['files', 52, 'conversation-files-parity'],
  ['guide', 16, 'guide', 'MINARBOT_GUIDE_SCREENSHOTS'],
  ['hub', 26, 'hub-parity', 'MINARBOT_HUB_SCREENSHOTS'],
  ['memory', 32, 'memory'],
  ['profiles', 64, 'agent-profile-parity'],
  ['projects', 68, 'project'],
  ['rich', 32, 'conversation-parity'],
  ['scoped', 52, 'conversation-automation-parity'],
  ['shell', 36, 'workspace-prompts'],
  ['system', 32, 'settings-system', 'MINARBOT_SYSTEM_SCREENSHOTS'],
];

const args = process.argv.slice(2);
if (args[0] === '--update') {
  const [, captureRoot, ...pairs] = args;
  assert(captureRoot && pairs.length && new Set(pairs).size === pairs.length,
    'Usage: npm run parity:gate -- --update <capture-root> group/name.png [group/name.png ...]');
  const updates = [];
  for (const pair of pairs) {
    assert(/^[a-z]+\/[a-zA-Z0-9_-]+\.png$/.test(pair), `Invalid pair: ${pair}`);
    const [group, name] = pair.split('/');
    assert(groups.some(([known]) => known === group), `Unknown group: ${group}`);
    const baseline = join(baselines, pair);
    assert((await stat(baseline)).isFile(), `Baseline missing: ${pair}`);
    const capture = (await actualImages(join(resolve(captureRoot), group), group)).get(name);
    assert(capture, `Capture missing: ${pair}`);
    const data = await sharp(capture).png({ compressionLevel: 9, adaptiveFiltering: true }).toBuffer();
    updates.push({ baseline, pair, data });
  }
  for (const { baseline, pair, data } of updates) {
    const staged = `${baseline}.${process.pid}.tmp`;
    await writeFile(staged, data, { flag: 'wx' });
    try {
      await rename(staged, baseline);
    } catch (error) {
      await unlink(staged);
      throw error;
    }
    console.log(`Updated ${pair}`);
  }
  process.exit(0);
}

assert(args.length <= 1 && !args[0]?.startsWith('--'), 'Pass an empty output directory.');
const cwd = resolve(import.meta.dirname, '../..');
const root = process.argv[2] ? resolve(process.argv[2]) : await mkdtemp(join(tmpdir(), 'minarbot-parity-'));
if (process.argv[2]) {
  await mkdir(root, { recursive: true });
  assert.equal((await readdir(root)).length, 0, `Parity output must be empty: ${root}`);
}
const reports = join(root, 'pairs');
await mkdir(reports, { recursive: true });
console.log(`Parity artifacts: ${root}`);

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

async function actualImages(dir, group) {
  const images = new Map();
  for (const file of (await readdir(dir)).sort()) {
    if (!png(file)) continue;
    const marker = /(^|-)leptos(?=-|\.png$)/;
    const name = file.replace(marker, '').replace(/^-/, '');
    assert(!images.has(name), `Duplicate screenshot: ${group}/${name}`);
    images.set(name, join(dir, file));
  }
  return images;
}

async function compare(left, right, output) {
  const [baseline, actual] = await Promise.all([left, right].map(path =>
    sharp(path).toColourspace('srgb').removeAlpha().raw().toBuffer({ resolveWithObject: true })));
  const { width, height } = baseline.info;
  const sameSize = width === actual.info.width && height === actual.info.height;

  await sharp({ create: { width: width + 8 + actual.info.width, height: Math.max(height, actual.info.height), channels: 3, background: '#181c24' } })
    .composite([{ input: left, left: 0, top: 0 }, { input: right, left: width + 8, top: 0 }]).png().toFile(output);
  if (!sameSize) return { changed: null, over16: null, box: '', result: 'dimensions' };
  let changed = 0, over16 = 0, minX = width, minY = height, maxX = 0, maxY = 0;
  for (let pixel = 0; pixel < width * height; pixel++) {
    const i = 3 * pixel;
    const delta = Math.max(Math.abs(baseline.data[i] - actual.data[i]), Math.abs(baseline.data[i + 1] - actual.data[i + 1]), Math.abs(baseline.data[i + 2] - actual.data[i + 2]));
    if (!delta) continue;
    changed++;
    if (delta > 16) over16++;
    const x = pixel % width, y = Math.floor(pixel / width);
    minX = Math.min(minX, x); minY = Math.min(minY, y);
    maxX = Math.max(maxX, x + 1); maxY = Math.max(maxY, y + 1);
  }
  const result = changed > maxChanged || over16 > maxOver16 ? 'pixels' : 'pass';
  return { changed, over16, box: changed ? `${minX},${minY},${maxX},${maxY}` : '', result };
}

const only = process.env.PARITY_GROUPS?.split(',').map(name => name.trim()).filter(Boolean);
if (only) {
  assert(only.length, 'PARITY_GROUPS must name at least one group.');
  const unknown = only.filter(name => !groups.some(([group]) => group === name));
  assert.equal(unknown.length, 0, `Unknown parity groups: ${unknown.join(', ')}`);
}
const selected = only ? groups.filter(([group]) => only.includes(group)) : groups;

const env = { ...process.env, MINARBOT_RUST_BINARY: resolve(process.env.MINARBOT_RUST_BINARY ?? join(cwd, '../target/debug/minarbot-server')) };
env.MINARBOT_HOSTED_HUB_BINARY ??= env.MINARBOT_RUST_BINARY;
const expectedGroups = groups.map(([group]) => group).sort();
assert.deepEqual((await readdir(baselines)).sort(), expectedGroups, 'Baseline group directories changed');
for (const key of ['MINARBOT_PROJECT_BENCH', 'MINARBOT_FILES_BENCH', 'MINARBOT_CONVERSATION_NAV_BENCH', 'MINARBOT_CONVERSATION_AUTOMATION_BENCH', 'MINARBOT_PROMPTS_BENCH', 'MINARBOT_AUTOMATION_WARM_BENCH']) env[key] ??= '0';
const rows = [], summary = [];
for (const [group, expectedCount, script, screenshotEnv] of selected) {
  const dir = join(root, group);
  await mkdir(dir);
  const settings = { ...env, ...(screenshotEnv ? { [screenshotEnv]: dir } : {}) };
  const check = await smoke(script, dir, settings, join(dir, 'smoke.log'));
  const actual = await actualImages(dir, group);
  const expectedDir = join(baselines, group);
  const expected = new Set(await readdir(expectedDir));
  const groupRows = [];
  const reportDir = join(reports, group);
  await mkdir(reportDir);
  for (const name of [...new Set([...expected, ...actual.keys()])].sort()) {
    const capture = actual.get(name);
    const compared = capture && expected.has(name)
      ? await compare(join(expectedDir, name), capture, join(reportDir, name))
      : { changed: null, over16: null, box: '', result: capture ? 'missing baseline' : 'missing capture' };
    groupRows.push({ pair: `${group}/${name}`, ...compared });
  }
  rows.push(...groupRows);
  const stats = { group, pairs: groupRows.filter(row => !row.result.startsWith('missing')).length,
    captures: actual.size, baselines: expected.size, expectedCount, failures: groupRows.filter(failed).length,
    maxChanged: Math.max(0, ...groupRows.map(row => row.changed ?? 0)),
    maxOver16: Math.max(0, ...groupRows.map(row => row.over16 ?? 0)), check };
  summary.push(stats);
  console.log(JSON.stringify(stats));
}
const header = 'pair\tchanged\tover16\tbox\tresult';
const tsv = entries => [header, ...entries.map(row => [row.pair, row.changed ?? '', row.over16 ?? '', row.box, row.result].join('\t')), ''].join('\n');
await writeFile(join(reports, 'all.tsv'), tsv(rows));
await writeFile(join(reports, 'nonidentical.tsv'), tsv(rows.filter(row => row.changed || row.result !== 'pass')));
await writeFile(join(reports, 'failures.tsv'), tsv(rows.filter(failed)));
await writeFile(join(reports, 'summary.json'), JSON.stringify(summary, null, 2) + '\n');
if (summary.some(group => group.failures || group.baselines !== group.expectedCount || group.captures !== group.expectedCount || group.check !== 0)) process.exitCode = 1;

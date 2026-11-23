import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdir, mkdtemp, readdir, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { baselineP95, HOST_BUSY_THRESHOLD, hostLoadAverage } from './timing-budgets.mjs';

const groups = {
  appearance: ['appearance', { APPEARANCE: 20 }, ['appearance']],
  automation: ['automation-parity', { AUTOMATION_COLD: 20, AUTOMATION_WARM: 80 }, ['automation routines', 'automation workflows', 'automation calendar']],
  controls: ['conversation-controls-parity', { CONVERSATION_NAV: 20 }, ['controls']],
  conversation: ['leptos-conversation', { CONVERSATION: 20 }, ['conversation']],
  files: ['conversation-files-parity', { FILES: 20 }, ['files']],
  hub: ['hub-parity', { HUB: 20 }, ['hub']],
  memory: ['memory', { MEMORY: 20 }, ['memory']],
  profiles: ['agent-profile-parity', { AGENT: 20 }, ['profiles']],
  projects: ['project', { PROJECT: 20 }, ['projects Overview', 'projects Connected apps', 'projects Environment', 'projects MCP']],
  scoped: ['conversation-automation-parity', { CONVERSATION_AUTOMATION: 20 }, ['scoped inbox', 'scoped remote', 'scoped upcoming', 'scoped workflow']],
  shell: ['workspace-prompts', { PROMPTS: 60 }, ['shell']],
};
assert.deepEqual(Object.values(groups).flatMap(([, , routes]) => routes).sort(), Object.keys(baselineP95).sort());
const selected = process.env.PARITY_GROUPS?.split(',').map(group => group.trim()) ?? Object.keys(groups);
assert(selected.every(group => Object.hasOwn(groups, group)) && new Set(selected).size === selected.length,
  'PARITY_GROUPS must contain unique timing group names');
assert(process.argv.length <= 3, 'Pass an empty output directory.');
const root = process.argv[2] ? resolve(process.argv[2]) : await mkdtemp(join(tmpdir(), 'minarbot-timing-'));
await mkdir(root, { recursive: true });
assert.equal((await readdir(root)).length, 0, 'Timing output directory must be empty.');
console.log(`Timing artifacts: ${root}`);

const load = [hostLoadAverage()];
const checks = [], measurements = [];
const timer = setInterval(() => load.push(hostLoadAverage()), 1000);
try {
  for (const group of selected) {
    if (Math.max(...load) > HOST_BUSY_THRESHOLD) break;
    const [script, flags, routes] = groups[group];
    const output = join(root, group);
    await mkdir(output);
    const report = join(output, 'timing.ndjson');
    await writeFile(report, '');
    const env = { ...process.env, MINARBOT_TIMING_REPORT: report };
    for (const [key, value] of Object.entries(flags)) env[`MINARBOT_${key}_BENCH`] = String(value);
    console.log(`Running ${group} at one-minute load ${load.at(-1).toFixed(2)}`);
    const code = await new Promise((done, reject) => {
      const child = spawn(process.execPath, ['--experimental-strip-types', join(import.meta.dirname, script + '-smoke.mjs'), output], {
        cwd: resolve(import.meta.dirname, '../..'), env, stdio: 'inherit',
      });
      child.once('error', reject);
      child.once('close', (code, signal) => done(code ?? signal));
    });
    const rows = (await readFile(report, 'utf8')).split('\n').filter(Boolean).map(line => JSON.parse(line));
    const expected = routes.flatMap(route => ['cold', 'warm'].map(phase => `${route} ${phase}`)).sort();
    const actual = rows.map(row => `${row.route} ${row.phase}`).sort();
    checks.push({ group, code, complete: JSON.stringify(expected) === JSON.stringify(actual) });
    measurements.push(...rows);
    load.push(hostLoadAverage());
  }
} finally {
  clearInterval(timer);
}
const peakLoad = Math.max(...load, ...measurements.map(row => row.load));
const busy = peakLoad > HOST_BUSY_THRESHOLD;
await writeFile(join(root, 'timing-summary.json'), JSON.stringify({ peakLoad, threshold: HOST_BUSY_THRESHOLD, busy, load, checks, measurements }, null, 2) + '\n');
if (busy) console.log(`host busy - peak one-minute load ${peakLoad.toFixed(2)} crossed ${HOST_BUSY_THRESHOLD.toFixed(2)}`);
console.table(measurements.map(({ route, phase, measured, budget, load }) => ({
  route, phase, p95: measured.toFixed(2), budget: budget.toFixed(2), load: load.toFixed(2),
  result: busy ? 'host busy' : measured > budget ? 'fail' : 'pass',
})));
process.exitCode = busy ? 2 : Number(checks.some(check => check.code !== 0 || !check.complete) ||
  measurements.some(row => row.measured > row.budget));

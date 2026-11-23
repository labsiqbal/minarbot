import { spawnSync } from 'node:child_process';
import { readdirSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { rustBinary } from '../rust-binary.ts';

function testFiles(root, prefix = '') {
  return readdirSync(join(root, prefix), { withFileTypes: true }).flatMap(entry => {
    const path = prefix + entry.name;
    if (entry.isDirectory()) return entry.name === 'node_modules' ? [] : testFiles(root, path + '/');
    // Keep the legacy HTTP suite, which predates the *.test.ts convention.
    return entry.isFile() && (path.endsWith('.test.ts') || path === 'test.ts') ? [path] : [];
  }).sort();
}

// Suites needing a real desktop or other external resources are opt-in by name.
const isOptIn = file => file.endsWith('.integration.test.ts');

export function discoverTests(root) {
  return testFiles(root).filter(file => !isOptIn(file));
}

export function assertTestCoverage(root, selected) {
  const scheduled = new Set(selected);
  for (const file of testFiles(root)) {
    if (!scheduled.has(file) && !isOptIn(file)) {
      throw new Error(`Test omitted: ${file}. Include it in discovery or rename it to *.integration.test.ts for opt-in.`);
    }
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const root = fileURLToPath(new URL('../', import.meta.url));
  const files = discoverTests(root);
  assertTestCoverage(root, files);
  if (!files.length) throw new Error('No tests discovered. Restore app test files before running npm test.');
  const server = rustBinary('minarbot-server', process.env.MINARBOT_RUST_BINARY);
  const { MINARBOT_HOSTED_HUB_BINARY: hosted = server, ...env } = process.env;
  env.MINARBOT_RUST_BINARY = server;
  const run = (tests, env) => {
    const result = spawnSync(process.execPath, ['--experimental-strip-types', '--test', '--test-concurrency=2', ...tests], { cwd: root, env, stdio: 'inherit' });
    if (result.error) throw result.error;
    return result.status ?? 1;
  };
  const node = run(files, env);
  const rust = run(['hub-link.test.ts'], { ...env, MINARBOT_HOSTED_HUB_BINARY: hosted });
  process.exitCode = node || rust;
}

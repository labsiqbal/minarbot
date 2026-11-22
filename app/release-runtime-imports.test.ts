import test from 'node:test';
import assert from 'node:assert/strict';
import { cpSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
import { rustBinary } from './rust-binary.ts';
import { fileURLToPath } from 'node:url';
import { xtask } from './test-support/release.ts';

test('release stages Rust runtime inputs and T3 notices without Node source', () => {
  const stage = mkdtempSync(join(tmpdir(), 'minarbot-runtime-'));
  try {
    const staged = spawnSync(xtask(), ['stage-runtime', fileURLToPath(new URL('./', import.meta.url)), stage], { encoding: 'utf8' });
    assert.equal(staged.status, 0, staged.stderr);
    assert.deepEqual(readdirSync(join(stage, 'app')), ['shared-data-worker.py']);
    assert.deepEqual(readFileSync(join(stage, 'app/shared-data-worker.py')), readFileSync(new URL('./shared-data-worker.py', import.meta.url)));
    assert.deepEqual(readdirSync(join(stage, 't3')).sort(), ['NOTICE', 'provenance.json']);
    assert.deepEqual(readdirSync(join(stage, 'scripts')), ['minarbot-command.sh']);
    mkdirSync(join(stage, 'bin'), { recursive: true });
    cpSync(rustBinary('minarbot-server', process.env.MINARBOT_RUST_BINARY), join(stage,'bin/minarbot-server'));
    const library = join(stage, 'library'); mkdirSync(library);
    const skills = spawnSync('npm', ['--silent', 'run', 'skills', '--', 'list', library], { cwd: stage, encoding: 'utf8' });
    assert.equal(skills.status, 0, skills.stderr);
    assert.deepEqual(JSON.parse(skills.stdout), []);
    const cli = spawnSync('npm', ['--silent', 'run', 'cli', '--', 'restore'], { cwd: stage, encoding: 'utf8' });
    assert.equal(cli.status, 2, cli.stderr);
    assert.match(JSON.parse(cli.stdout).error, /^Use restore workspace/);

  } finally { rmSync(stage, { recursive: true, force: true }); }
});

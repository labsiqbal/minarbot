import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { copyFile, mkdir, mkdtemp, readdir, readFile, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import sharp from 'sharp';

test('golden gate compares exact inventories and enforces dimensions and pixel ceilings', async () => {
  const scratch = await mkdtemp(join(tmpdir(), 'minarbot-golden-test-'));
  try {
    const baselines = join(scratch, 'baselines');
    for (const group of await readdir(join(import.meta.dirname, 'baselines'))) {
      await mkdir(join(baselines, group), { recursive: true });
    }
    const gate = join(scratch, 'parity-gate.mjs');
    await copyFile(join(import.meta.dirname, 'parity-gate.mjs'), gate);
    await symlink(join(import.meta.dirname, '../../node_modules'), join(scratch, 'node_modules'));
    await writeFile(join(scratch, 'leptos-conversation-smoke.mjs'), `
      import { cp } from 'node:fs/promises';
      await cp(new URL('./fixture', import.meta.url), process.argv[2], { recursive: true });
    `);
    const images = join(scratch, 'fixture');
    await mkdir(images);
    const png = (width, raw) => sharp(raw, { raw: { width, height: 1, channels: 3 } }).png().toBuffer();
    const pixels = Buffer.alloc(101 * 3);
    const baseline = await png(101, pixels);
    const baselineDir = join(baselines, 'conversation');
    const names = ['equal.png', 'allowance.png', 'edges.png', 'fourth.png'];
    for (const name of names) await writeFile(join(baselineDir, name), baseline);
    await writeFile(join(images, 'equal.png'), baseline);
    pixels.fill(17, 0, 24 * 3);
    pixels.fill(16, 24 * 3, 100 * 3);
    await writeFile(join(images, 'allowance.png'), await png(101, pixels));
    await writeFile(join(images, 'edges.png'), baseline);
    await writeFile(join(images, 'fourth.png'), baseline);
    const run = async name => {
      const output = join(scratch, name);
      const result = spawnSync(process.execPath, [gate, output], { encoding: 'utf8', env: { ...process.env, PARITY_GROUPS: 'conversation' } });
      assert.ifError(result.error);
      return { ...result, failures: await readFile(join(output, 'pairs/failures.tsv'), 'utf8') };
    };
    const passing = await run('pass');
    assert.equal(passing.status, 0, passing.stderr);
    assert.equal(passing.failures.trim().split('\n').length, 1);
    pixels[24 * 3] = 17;
    await writeFile(join(images, 'allowance.png'), await png(101, pixels));
    await writeFile(join(images, 'edges.png'), await png(100, Buffer.alloc(300)));
    await rm(join(images, 'fourth.png'));
    await writeFile(join(images, 'extra.png'), baseline);
    const failing = await run('fail');
    assert.equal(failing.status, 1, failing.stderr);
    assert.match(failing.failures, /allowance\.png\t100\t25\t0,0,100,1\tpixels/);
    assert.match(failing.failures, /edges\.png\t\t\t\tdimensions/);
    assert.match(failing.failures, /extra\.png\t\t\t\tmissing baseline/);
    assert.match(failing.failures, /fourth\.png\t\t\t\tmissing capture/);
  } finally { await rm(scratch, { recursive: true }); }
});

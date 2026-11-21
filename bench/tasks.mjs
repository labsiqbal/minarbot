import assert from 'node:assert/strict';
import { readdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';

const root = join(import.meta.dirname, 'tasks');
const read = path => readFile(path, 'utf8');
export const tasks = await Promise.all((await readdir(root)).sort().map(async id => {
    assert.match(id, /^[a-z][a-z0-9-]+$/);
    const dir = join(root, id), { category, timeSeconds: seconds, tokenBudget: tokens } = JSON.parse(await read(join(dir, 'task.json')));
    const prompt = (await read(join(dir, 'prompt.md'))).trim();
    assert(category && prompt && Number.isSafeInteger(seconds) && seconds > 0 && Number.isSafeInteger(tokens) && tokens > 0, `Invalid task: ${id}`);
    const seedDir = join(dir, 'seed');
    const names = await readdir(seedDir).catch(error => { if (error.code !== 'ENOENT') throw error; return []; });
    const seed = Object.fromEntries(await Promise.all(names.sort().map(async name => [name, await read(join(seedDir, name))])));
    return { id, category, prompt, seed, seconds, tokens };
}));

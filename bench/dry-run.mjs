import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { checkTask } from './checkers.mjs';

const moduleTest = (file, tests) =>
    `import assert from 'node:assert/strict'; import test from 'node:test'; import * as m from './${file}'; test('regression', () => { ${tests} });`;
const launch = `<!doctype html><html lang="en"><title>Atlas launch</title><h1>Atlas launch</h1>
<table><caption>Checklist</caption><thead><tr><th scope="col">Task</th><th scope="col">Date</th></tr></thead><tbody>
<tr><td>Migration</td><td>2026-11-25</td></tr><tr><td>QA</td><td>2026-11-27</td></tr>
<tr><td>Launch</td><td>2026-12-01</td></tr></tbody></table>`;

const outputs = {
    'counter-app': {
        'index.html': `<!doctype html><html lang="en"><title>Counter</title><h1>Counter</h1>
<output id="count">0</output><button type="button">Add</button><button type="button">Reset</button>
<script type="module">
import { increment, reset } from './counter.mjs';
const count = document.querySelector('#count'), [add, clear] = document.querySelectorAll('button');
add.onclick = () => { count.value = increment(Number(count.value)); };
clear.onclick = () => { count.value = reset(); };
</script>`,
        'counter.mjs': 'export const increment = value => value + 1; export const reset = () => 0;',
        'counter.test.mjs': moduleTest('counter.mjs', 'assert.equal(m.increment(9), 10); assert.equal(m.reset(), 0);'),
    },
    'tip-app': {
        'index.html': `<!doctype html><html lang="en"><title>Tip calculator</title><h1>Tip calculator</h1>
<label for="bill">Bill</label><input type="number" id="bill" min="0" step="0.01">
<label for="percent">Percent</label><input type="number" id="percent" min="0">
<button type="button">Calculate</button><output id="total" aria-live="polite"></output>
<script type="module">
import { total } from './tip.mjs';
const output = document.querySelector('#total');
document.querySelector('button').onclick = () => {
    try {
        output.value = total(Number(document.querySelector('#bill').value), Number(document.querySelector('#percent').value)).toFixed(2);
    } catch {
        output.value = 'Enter nonnegative values.';
    }
};
</script>`,
        'tip.mjs': 'export function total(bill, percent) { if (bill < 0 || percent < 0) throw new RangeError(); return bill * (100 + percent) / 100; }',
        'tip.test.mjs': moduleTest('tip.mjs', 'assert.equal(m.total(100, 15), 115); assert.throws(() => m.total(-1, 5));'),
    },
    'cart-bug': {
        'cart.mjs': 'export function total(items) { return Math.round(items.reduce((sum, item) => sum + item.price * item.quantity, 0) * 100) / 100; }',
        'cart.test.mjs': moduleTest('cart.mjs', 'assert.equal(m.total([{price: 2.25, quantity: 3}]), 6.75);'),
    },
    'expiry-bug': {
        'auth.mjs': 'export const isExpired = (expiresAt, now) => expiresAt != null && now >= expiresAt;',
        'auth.test.mjs': moduleTest('auth.mjs', 'assert.equal(m.isExpired(100, 100), true);'),
    },
    'slug-feature': {
        'text.mjs': "export const greet = name => 'Hello, ' + name + '!'; export const slugify = text => text.toLowerCase().trim().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');",
        'text.test.mjs': moduleTest('text.mjs', "assert.equal(m.slugify(' Hi, World! '), 'hi-world');"),
    },
    'paginate-feature': {
        'list.mjs': `export const first = items => items[0]; export function paginate(items, page, size) {
    if (![page, size].every(value => Number.isInteger(value) && value > 0)) throw new RangeError();
    return { items: items.slice((page - 1) * size, page * size), total: items.length, pages: Math.ceil(items.length / size) };
}`,
        'list.test.mjs': moduleTest('list.mjs', 'assert.deepEqual(m.paginate([1, 2], 2, 1), {items: [2], total: 2, pages: 2});'),
    },
    'contacts-csv': {
        'contacts.json': '[{"name":"Alice","email":"alice@example.com"},{"name":"Bob","email":"bob@example.com"},{"name":"Doe, Jane","email":"jane@example.com"}]',
        'summary.json': '{"inputRows":5,"outputRows":3,"droppedRows":2}',
    },
    'sales-csv': { 'sales-summary.json': '{"currency":"USD","paidOrders":3,"paidTotal":35.30,"byRegion":{"West":26.80,"East":8.50}}' },
    'http-research': {
        'http-facts.json': JSON.stringify([
            { status: 200, title: 'OK', source: 'https://www.rfc-editor.org/rfc/rfc9110.html#section-15.3.1' },
            { status: 404, title: 'Not Found', source: 'https://www.rfc-editor.org/rfc/rfc9110.html#section-15.5.5' },
            { status: 503, title: 'Service Unavailable', source: 'https://www.rfc-editor.org/rfc/rfc9110.html#section-15.6.4' },
        ]),
    },
    'csv-research': {
        'csv-facts.json': '{"mediaType":"text/csv","extension":"csv","rfc":4180,"source":"https://www.iana.org/assignments/media-types/text/csv"}',
    },
    'browser-booking': {},
    'browser-ticket': {},
    'handover-doc': {
        'handover.md': `# Summary
Atlas launches 2026-12-01 within a budget of USD 2400.
# Decisions
Keep email login.
# Actions
- Maya owns migration by 2026-11-25.
- Rafi owns QA by 2026-11-27.
# Risks
The vendor sandbox is unavailable. Test with the local fixture.`,
    },
    'team-review': { 'review.json': '{"subtotal":100,"tax":10,"total":110,"approved":true}' },
    'show-html': { 'launch.html': launch },
    'routine-setup': {},
};

export async function dryRun(task) {
    const workspace = await mkdtemp(join(tmpdir(), 'minarbot-dry-check-'));
    try {
        const fixture = outputs[task.id];
        if (!fixture) throw Error(`Missing known-good fixture for ${task.id}`);
        for (const [name, value] of Object.entries({ ...task.seed, ...fixture }))
            await writeFile(join(workspace, name), value);
        await checkTask(task, {
            workspace,
            shown: true,
            shownHtml: launch,
            delegation: { delegated: true, reviewerDone: true },
            routines: [{ name: 'Daily sales', enabled: 0, trigger: 'manual', prompt: 'Read sales.csv and report paid revenue.', report_to: 'direct' }],
            routineRuns: 0,
            browser: {
                submission: task.id === 'browser-booking'
                    ? { choice: 'Team room', text: 'Friday planning', confirmed: true }
                    : { choice: 'High', text: 'Printer offline', confirmed: true },
                steps: ['review', 'confirm'],
                tools: ['computer_open_url', 'computer_screenshot', 'computer_click', 'computer_type'],
            },
        });
    } finally {
        await rm(workspace, { recursive: true, force: true });
    }
}

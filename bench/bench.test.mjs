import assert from 'node:assert/strict';
import test from 'node:test';
import { checkData } from './checkers.mjs';
import { dryRun } from './dry-run.mjs';
import { tasks } from './tasks.mjs';

test('all benchmark categories have a known-good fixture', () => {
    assert(tasks.length >= 15 && tasks.length <= 20);
    for (const category of ['web-app', 'bug-fix', 'csv', 'research', 'browser', 'document', 'delegation'])
        assert(tasks.some(task => task.category === category), category);
});

for (const task of tasks) test(task.id, () => dryRun(task));

test('checkers reject unsupported sources and missing Computer or delegation evidence', () => {
    assert.throws(() => checkData('csv-research', { mediaType: 'text/csv', extension: 'csv', rfc: 4180, source: 'https://example.test/csv' }));
    assert.throws(() => checkData('browser-ticket', { submission: { choice: 'High', text: 'Printer offline', confirmed: true }, steps: ['review', 'confirm'], tools: [] }));
    assert.throws(() => checkData('team-review', { output: { subtotal: 100, tax: 10, total: 110, approved: true }, delegated: false, reviewerDone: true }));
});

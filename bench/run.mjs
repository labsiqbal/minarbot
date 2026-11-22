import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { mkdir, mkdtemp, readFile, rename, rm, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { parseArgs } from 'node:util';
import { pathToFileURL } from 'node:url';
import { checkTask } from './checkers.mjs';
import { dryRun } from './dry-run.mjs';
import { fixtureSite } from './fixture.mjs';
import { startServer } from './server.mjs';
import { tasks } from './tasks.mjs';

const here = import.meta.dirname, root = join(here, '..');
const terminal = new Set(['done', 'failed', 'cancelled', 'interrupted']);
const native = new Set(['codex', 'claude']);
const proseQuestion = /\b(?:would|could|can) you (?:please )?(?:confirm|provide|clarify|choose|tell)\b[^\n]*\?|\b(?:which|what) (?:file|format|option|time|name|model|runtime|budget)\b[^.\n]*\?/i;

function seedGit(workspace) {
    for (const args of [['init', '-q', '--template=', '-b', 'main'], ['add', '.'], ['commit', '-qm', 'Seed']]) {
        execFileSync('/usr/bin/git', ['-c', 'core.hooksPath=/dev/null', '-c', 'user.name=Minarbot Benchmark',
            '-c', 'user.email=benchmark@example.test', ...args], {
            cwd: workspace, stdio: 'ignore', env: { PATH: '/usr/bin:/bin', GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: '/dev/null' },
        });
    }
}

async function disposable(callback) {
    // Same filesystem as nested bwrap mounts, with private in-memory data and transcripts.
    const scratch = await mkdtemp('/dev/shm/minarbot-bench-');
    try { return await callback(scratch); } finally { await rm(scratch, { recursive: true, force: true }); }
}

function providerConfig() {
    const { MINARBOT_BENCH_API_URL: baseURL, MINARBOT_BENCH_API_MODEL: defaultModel, MINARBOT_BENCH_API_KEY: apiKey } = process.env;
    if (!baseURL && !defaultModel && !apiKey) return undefined;
    assert(baseURL && defaultModel, 'Set MINARBOT_BENCH_API_URL and MINARBOT_BENCH_API_MODEL together');
    const url = new URL(baseURL);
    assert(!url.username && !url.password && !url.search && !url.hash, 'Provider URL must have no credentials, query or fragment');
    assert(url.protocol === 'https:' || url.protocol === 'http:' && ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname),
        'Provider must use HTTPS or loopback HTTP');
    return { id: 'benchmark', name: 'Benchmark', provider: 'openai-compatible', baseURL, defaultModel, apiKey };
}

function safeCheckerError(error) {
    if (error.code === 'ENOENT') return 'Required output missing';
    if (error.code === 'ELOOP') return 'Output must be a regular file';
    if (error.code === 'ERR_ASSERTION') return error.generatedMessage ? 'Output differs from expected result' : error.message;
    if (error instanceof SyntaxError) return 'Output is invalid JSON';
    if (error.killed || error.signal) return 'Checker timed out or was interrupted';
    return 'Checker process failed';
}

function observations(db) {
    const turns = db.prepare('SELECT id, agent, channel, thread, status, paused, reply FROM turns ORDER BY seq').all();
    const calls = db.prepare('SELECT turn, model, phase, input_tokens, output_tokens, total_tokens FROM provider_calls ORDER BY seq').all();
    const n = query => db.prepare(query).get().n;
    const hasRecall = n("SELECT count(*) AS n FROM sqlite_master WHERE type='table' AND name='memory_recall_metrics'");
    const recall = hasRecall ? db.prepare('SELECT added_tokens,units FROM memory_recall_metrics WHERE turn=?') : null;
    const turnUsage = turns.map(turn => {
        const context = recall?.get(turn.id);
        const usage = calls.filter(call => call.turn === turn.id).map(({ phase, input_tokens, output_tokens, total_tokens }) => ({
            phase, inputTokens: input_tokens, outputTokens: output_tokens, tokens: total_tokens,
        }));
        return { autoRecallTokenUpperBound: context?.added_tokens ?? 0, autoRecalledUnits: context?.units ?? 0, usage };
    });
    const interventions = {
        approvals: n(`SELECT (SELECT count(*) FROM harness_approvals)
            + (SELECT count(*) FROM composio_calls WHERE decision IS NOT NULL OR state='approval') AS n`),
        questions: n('SELECT count(*) AS n FROM questions'),
        computer: n('SELECT count(*) AS n FROM computer_waits'),
        prose: turns.filter(row => proseQuestion.test(row.reply ?? '')).length,
    };
    const knownTokens = calls.reduce((sum, call) => sum + (call.total_tokens ?? 0), 0);
    const unknownUsage = calls.filter(call => call.total_tokens === null).length;
    return {
        turns, turnUsage, interventions,
        interventionCount: Object.values(interventions).reduce((sum, count) => sum + count, 0),
        knownTokens, tokens: calls.length && !unknownUsage ? knownTokens : null, unknownUsage,
        providerCalls: calls.length, models: [...new Set(calls.map(call => call.model))],
    };
}

async function attachedFiles(app, rows) {
    const artifacts = Object.create(null), htmlFiles = new Map();
    let shownHtml = null;
    for (const [scope, row] of rows) {
        for (const file of row.generatedFiles ?? []) {
            if (file.deleted || file.size > 256 * 1024) continue;
            const path = `${scope}/files/${file.id}`;
            const response = await app.http(path);
            if (response.ok) {
                artifacts[file.name] = await response.text();
                if (file.name === 'launch.html') htmlFiles.set(path, artifacts[file.name]);
            } else await response.body?.cancel();
        }
        for (const match of (row.reply ?? '').matchAll(/\[[^\]]*\]\(([^)]+)\)/g)) {
            const url = URL.parse(match[1], app.origin);
            if (!url) continue;
            if (url.origin !== app.origin || !/^\/(?:sites\/|api\/(?:agents|channels|threads)\/[^/]+\/(?:panel\/)?(?:workspace\/file|files\/))/.test(url.pathname))
                continue;
            if (htmlFiles.has(url.pathname)) {
                shownHtml = htmlFiles.get(url.pathname);
                continue;
            }
            const response = await app.http(url.pathname + url.search);
            if (response.ok && response.headers.get('content-type')?.startsWith('text/html')) shownHtml = await response.text();
            else await response.body?.cancel();
        }
    }
    return { artifacts, shown: shownHtml !== null, shownHtml };
}

function turnScope(row) {
    return row.channel ? `/api/channels/${row.channel}` : row.thread ? `/api/threads/${row.thread}` : `/api/agents/${row.agent}`;
}

async function attempt(runtime, task, count, server, signal) {
    const row = {
        task: task.id, category: task.category, attempt: count,
        budget: { seconds: task.seconds, tokens: task.tokens }, status: 'fail',
        reason: null, checker: null, turns: null, wallSeconds: null, tokens: null, knownTokens: null,
        providerCalls: null, unknownUsage: null, models: [], interventions: null, interventionKinds: null, costUSD: null,
    };
    if (runtime.reason) return { ...row, status: 'skipped', reason: runtime.reason };
    return disposable(async scratch => {
        const lead = randomUUID(), reviewer = task.category === 'delegation' ? randomUUID() : null;
        const workspace = join(scratch, 'home', 'workspace');
        await mkdir(workspace, { recursive: true });
        for (const [name, value] of Object.entries(task.seed)) await writeFile(join(workspace, name), value);
        if (task.category === 'bug-fix' || task.category === 'feature') seedGit(workspace);
        const site = task.category === 'browser' ? await fixtureSite() : null;
        let app, db;
        try {
            app = await startServer({ ...server, scratch, fixture: site });
            const { DatabaseSync } = await import('node:sqlite');
            db = new DatabaseSync(app.database, { readOnly: true, timeout: 1000 });
            const model = native.has(runtime.name) ? 'default' : runtime.model;
            const account = native.has(runtime.name) ? 'device' : server.provider?.id;
            const profile = {
                runtime: runtime.name, model, ...(account && { providerAccount: account }),
                instructions: 'Complete the user task in the supplied workspace. Verify the result and deliver requested files.',
            };
            await app.api('/api/agents', { id: lead, name: 'Builder', ...profile });
            let scope = `/api/agents/${lead}`;
            await app.api(scope + '/folder', { id: randomUUID(), revision: 0, path: workspace, mode: 'read-write' });
            if (reviewer) {
                await app.api('/api/agents', {
                    ...profile, id: reviewer, name: 'Reviewer',
                    instructions: 'Check the requested arithmetic against quote.json in the project workspace. Reply to Builder with your findings.',
                });
                await app.api(`/api/agents/${reviewer}/folder`, { id: randomUUID(), revision: 0, path: workspace, mode: 'read-only' });
                const project = await app.api('/api/projects', {
                    id: randomUUID(), name: 'Quote review', folders: [{ mode: 'existing', path: workspace }],
                    members: [{ agent: lead, write: true }, { agent: reviewer, write: false }], confirmWrite: true,
                });
                scope = `/api/channels/${project.sections[0].id}`;
            }
            if (server.projectCodegraph && ['bug-fix', 'feature'].includes(task.category)) {
                const project = await app.api('/api/projects', {
                    id: randomUUID(), name: 'Benchmark project', folders: [{ mode: 'existing', path: workspace }],
                    members: [{ agent: lead, write: true }], confirmWrite: true,
                });
                const revision = project.codegraphEnabled ? project.revision : (await app.api(
                    `/api/projects/${project.id}/edit`,
                    { id: randomUUID(), revision: project.revision, codegraphEnabled: true },
                )).revision;
                const thread = await app.api(`/api/projects/${project.id}/thread`, {
                    id: randomUUID(), revision, agent: lead, folder: workspace, name: 'Benchmark',
                });
                scope = `/api/threads/${thread.threads[0].id}`;
            }
            if (site) {
                const path = `${scope}/computer`, current = await app.api(path);
                await app.api(path, { id: randomUUID(), revision: current.revision, enabled: true });
                const response = await app.http(path + '/start', {}, 75000);
                if (response.status === 503 || response.status === 410) {
                    await response.body?.cancel();
                    return { ...row, status: 'skipped', reason: `Computer unavailable in disposable server (HTTP ${response.status})` };
                }
                await response.body?.cancel();
                if (!response.ok) throw Error(`Computer start returned HTTP ${response.status}`);
            }

            const id = randomUUID(), started = performance.now(), deadline = started + task.seconds * 1000;
            signal.throwIfAborted();
            await app.api(scope + '/turns', { id, text: task.prompt.replaceAll('{{fixture}}', site?.publicOrigin ?? ''), ...(reviewer && { recipient: lead }) });
            for (;;) {
                const state = observations(db);
                if (state.interventionCount) row.reason = 'User intervention requested';
                else if (signal.aborted) row.reason = 'Runner interrupted';
                else if (performance.now() >= deadline) row.reason = 'Wall-time budget exceeded';
                else if (state.knownTokens > task.tokens) row.reason = 'Token budget exceeded';
                else if (state.turns.length && state.turns.every(turn => terminal.has(turn.status))) {
                    const failed = state.turns.find(turn => turn.status !== 'done');
                    if (failed) row.reason = `Runtime turn ${failed.status}`;
                    break;
                }
                if (row.reason) break;
                await delay(250);
            }
            row.wallSeconds = Math.round((performance.now() - started) / 10) / 100;
            const state = observations(db);
            for (const turn of state.turns.filter(turn => !terminal.has(turn.status))) {
                await app.api(`${turnScope(turn)}/turns/${turn.id}/cancel`, { id: randomUUID() }).catch(() => {});
            }
            const final = observations(db);
            row.turns = final.turns.length;
            row.turnUsage = final.turnUsage;
            row.tokens = final.tokens;
            row.knownTokens = final.knownTokens;
            row.providerCalls = final.providerCalls;
            row.unknownUsage = final.unknownUsage;
            row.models = final.models;
            row.interventions = final.interventionCount;
            row.interventionKinds = final.interventions;
            if (final.interventionCount) row.reason ??= 'User intervention requested';
            if (final.knownTokens > task.tokens) row.reason ??= 'Token budget exceeded';

            if (!row.reason || state.turns.every(turn => terminal.has(turn.status))) {
                try {
                    const context = { workspace };
                    if (task.id === 'show-html' || !['web-app', 'bug-fix', 'feature', 'routine', 'browser'].includes(task.category)) {
                        const scopes = [...new Set(final.turns.map(turnScope))];
                        const history = await Promise.all(scopes.map(async path => (await app.api(path + '/turns')).map(turn => [path, turn])));
                        Object.assign(context, await attachedFiles(app, history.flat()));
                    }
                    if (reviewer) {
                        const deliveries = db.prepare(`SELECT t.id FROM agent_deliveries d JOIN agent_requests r ON r.id=d.request
                            JOIN turns p ON p.id=r.turn JOIN turns t ON t.id=d.turn WHERE p.agent=? AND t.agent=?
                            UNION SELECT resolved FROM room_invitations i JOIN turns p ON p.id=i.source WHERE p.agent=? AND i.target=?`)
                            .all(lead, reviewer, lead, reviewer);
                        context.delegation = { delegated: deliveries.length > 0,
                            reviewerDone: final.turns.some(turn => turn.agent === reviewer && turn.status === 'done'
                                && !!turn.reply?.trim() && deliveries.some(delivery => delivery.id === turn.id)) };
                    }
                    if (task.category === 'routine') {
                        context.routines = db.prepare('SELECT name, enabled, trigger, prompt, report_to FROM routines WHERE agent=?').all(lead);
                        context.routineRuns = db.prepare('SELECT count(*) AS n FROM routine_runs').get().n;
                    }
                    if (site) {
                        const tools = db.prepare(`SELECT json_extract(call,'$.function.name') AS name FROM flow_tool_calls
                            UNION SELECT name FROM harness_steps WHERE state='done'`).all();
                        context.browser = { submission: site.submissions[task.id.split('-')[1]], steps: site.steps,
                            tools: tools.map(tool => tool.name?.replace(/^mcp__minarbot__/, '')) };
                    }
                    await checkTask(task, context);
                    row.checker = { passed: true };
                } catch (error) {
                    row.checker = { passed: false, reason: safeCheckerError(error) };
                }
            }
            row.reason ??= row.checker?.reason ?? (row.tokens === null ? 'Token usage unavailable' : null);
            row.status = row.reason ? 'fail' : 'pass';
            return row;
        } finally {
            db?.close();
            try { await app?.close(); } finally { await site?.close(); }
        }
    });
}

function scoreboard(receipt) {
    const rows = receipt.results;
    const pass = rows.filter(row => row.status === 'pass').length, fail = rows.filter(row => row.status === 'fail').length;
    return [
        `# ${receipt.runtime.name} benchmark`, '',
        `Minarbot ${receipt.version} · ${receipt.startedAt} · ${receipt.runtime.version ?? 'no CLI version'}`, '',
        `**${pass} passed / ${pass + fail} attempted** · ${rows.length - pass - fail} skipped · ${rows.reduce((n, row) => n + (row.interventions ?? 0), 0)} recorded interventions`, '',
        '| Task | Attempt | Status | Turns | Tokens | Seconds | Interventions | Result |',
        '| --- | ---: | --- | ---: | ---: | ---: | ---: | --- |',
        ...rows.map(row => `| ${row.task} | ${row.attempt} | ${row.status} | ${row.turns ?? '-'} | ${row.tokens ?? (row.knownTokens == null ? '-' : `≥${row.knownTokens}`)} | ${row.wallSeconds ?? '-'} | ${row.interventions ?? '-'} | ${(row.reason ?? '').replaceAll('|', '\\|').replaceAll('\n', ' ')} |`),
        '', 'USD cost unavailable. Minarbot does not report billing through its usage ledger.', '',
        ...rows.filter(row => row.recheck).map(row =>
            `- ${row.task} rechecked from the original attempt using checker commit \`${row.recheck.checkerCommit.slice(0, 12)}\`.`),
        '',
    ].join('\n');
}

export async function save(dir, receipt) {
    for (const [extension, body] of [['json', JSON.stringify(receipt, null, 2) + '\n'], ['md', scoreboard(receipt)]]) {
        const path = join(dir, receipt.runtime.name + '.' + extension);
        await writeFile(path + '.tmp', body, { mode: 0o600 });
        await rename(path + '.tmp', path);
    }
}

async function main() {
    const { values } = parseArgs({ options: {
        'dry-run': { type: 'boolean' }, 'project-codegraph': { type: 'boolean' }, tasks: { type: 'string' }, runtime: { type: 'string' },
        repeat: { type: 'string', default: '1' }, binary: { type: 'string', default: join(root, 'target/release/minarbot-server') },
        results: { type: 'string', default: join(here, 'results') },
    } });
    const taskIds = values.tasks?.split(',') ?? tasks.map(task => task.id);
    const selected = tasks.filter(task => taskIds.includes(task.id));
    assert(taskIds.every(id => selected.some(task => task.id === id)), 'Unknown task ID in --tasks');
    const repeat = Number(values.repeat);
    assert(/^\d+$/.test(values.repeat) && Number.isSafeInteger(repeat) && repeat > 0, '--repeat must be a positive integer');
    if (values['dry-run']) {
        for (const task of selected) {
            await dryRun(task);
            console.log(`${task.id}: fixture passed`);
        }
        return;
    }

    const server = { binary: resolve(values.binary), dist: join(root, 'app/dist'), provider: providerConfig(), projectCodegraph: values['project-codegraph'] };
    const hash = createHash('sha256');
    for await (const chunk of createReadStream(server.binary)) hash.update(chunk);
    const binarySHA256 = hash.digest('hex');
    const startedAt = new Date().toISOString();
    const checkoutCommit = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim();
    const suiteSHA256 = createHash('sha256').update(JSON.stringify(tasks)).update(await readFile(join(here, 'checkers.mjs')))
        .update(await readFile(join(here, 'fixture.mjs'))).digest('hex');
    const { version } = JSON.parse(await readFile(join(root, 'app/package.json'), 'utf8'));
    assert.match(version, /^[0-9][0-9A-Za-z.-]*$/);
    const choices = await disposable(async scratch => {
        const app = await startServer({ ...server, scratch });
        try {
            const environment = await app.api('/api/environment');
            assert.equal(environment.version, version, 'Release binary and checkout versions differ');
            return await app.api('/api/runtimes', undefined, 65000);
        } finally {
            await app.close();
        }
    });
    const inventory = [
        { name: 'api', model: server.provider?.defaultModel, reason: server.provider ? null : 'skipped: no provider' },
        ...choices.choices.map(({ runtime: name, version, available, signedIn, models, reason }) => ({
            name, version, account: native.has(name) ? 'device' : server.provider?.id,
            model: native.has(name) ? 'default' : models[0]?.id ?? server.provider?.defaultModel ?? 'default',
            reason: !available ? reason ?? 'Harness unavailable' : signedIn === false ? 'Device sign-in unavailable'
                : native.has(name) || models.length > 0 || server.provider ? null : 'No ACP model reported and no provider configured',
        })),
    ];
    const requested = values.runtime?.split(',') ?? inventory.map(runtime => runtime.name);
    assert(requested.every(name => inventory.some(runtime => runtime.name === name)), 'Unknown runtime in --runtime');
    const dir = resolve(values.results, version, startedAt.replaceAll(':', '-'));
    await mkdir(dir, { recursive: true, mode: 0o700 });
    const controller = new AbortController();
    const stop = () => controller.abort(Error('Runner interrupted'));
    process.on('SIGINT', stop).on('SIGTERM', stop);
    try {
        for (const runtime of inventory.filter(runtime => requested.includes(runtime.name))) {
            const receipt = { version, binarySHA256, checkoutCommit, suiteSHA256, startedAt, runtime, repeat, projectCodegraph: !!server.projectCodegraph, complete: false, results: [] };
            for (let count = 1; count <= repeat && !controller.signal.aborted; count++) {
                for (const task of selected) {
                    if (controller.signal.aborted) break;
                    let row;
                    try {
                        row = await attempt(runtime, task, count, server, controller.signal);
                    } catch (error) {
                        row = { task: task.id, category: task.category, attempt: count, budget: { seconds: task.seconds, tokens: task.tokens },
                            status: 'fail', reason: /^(HTTP |Computer start |Disposable server )/.test(error.message) ? error.message : 'Runner setup or observation failed' };
                    }
                    receipt.results.push(row);
                    console.log(`${runtime.name}/${task.id} #${count}: ${row.status}${row.reason ? ` (${row.reason})` : ''}`);
                    await save(dir, receipt);
                }
            }
            receipt.complete = !controller.signal.aborted;
            receipt.finishedAt = new Date().toISOString();
            await save(dir, receipt);
            if (controller.signal.aborted) { process.exitCode = 130; break; }
        }
    } finally {
        process.off('SIGINT', stop).off('SIGTERM', stop);
    }
    console.log(`Scoreboards: ${dir}`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) await main();

import type { ChildProcess } from 'node:child_process';
import { once, EventEmitter } from 'node:events';
import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer, request as httpRequest } from 'node:http';
import type { SessionIdentity } from './hub/sessions.ts';
import { generateKeyPairSync, randomUUID, createPublicKey } from 'node:crypto';
import { mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { testDatabase } from './test-support/database.ts';
import { hubCommand, startRustHub } from './test-support/hosted-hub.ts';
import { rustBinary } from './rust-binary.ts';
import type { HubLinkOptions } from './rust-hub-test-bridge.ts';
import WebSocket from 'ws';
import { startApp } from './test-bridge.ts';
import { signedNodeMessage, verifiedNodeMessage } from './hub/node-link.ts';
import { createHubAuthority, type ParticipationPolicy, type InvocationPolicy } from './hub/project-authority.ts';
import { createHubWebApi } from './hub/web.ts';
import { createHubRouting, type RoutingFrame } from './hub/routing.ts';
import { canonicalJson, ProtocolError, sha256 } from './shared/protocol.ts';
type App = Awaited<ReturnType<typeof startApp>>;
const sleep = (ms: number) => new Promise(resolve => setTimeout(resolve, ms));
async function until(check: () => unknown | Promise<unknown>, label = 'condition') {
    const end = Date.now() + 12000;
    while (Date.now() < end) {
        if (await check())
            return;
        await sleep(25);
    }
    assert.fail(`Timed out: ${label}`);
}
async function api(app: App, path: string, body?: unknown, status = 200): Promise<any> { const response = await fetch(app.origin + path, { method: body === undefined ? 'GET' : 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json', Connection: 'close' }, body: body === undefined ? undefined : JSON.stringify(body) }); const result = await response.json(); assert.equal(response.status, status, JSON.stringify(result)); return result; }
const who = (subject: string) => ({ issuer: 'https://identity.test', subject, verifiedMailboxes: [`${subject}@example.test`] });
async function fixture(t: test.TestContext, providerScript?: (body: any) => unknown | Promise<unknown>, stream = false) {
    const dir = mkdtempSync(join(tmpdir(), 'minarbot-hub-link-'));
    const dataPath = join(dir, 'hub.sqlite');
    let clock = Date.now();
    const binary = rustBinary('minarbot-server', process.env.MINARBOT_HOSTED_HUB_BINARY ?? process.env.MINARBOT_RUST_BINARY);
    const now = () => clock;
    const signer = generateKeyPairSync('ed25519').privateKey;
    const hubKey = createPublicKey(signer).export({ type: 'spki', format: 'der' }).toString('base64url');
    const accountsHost = 'accounts.minarbot.test', origin = `https://${accountsHost}`;
    const subjects = ['alice', 'bob', 'eve'] as const;
    const identities = Object.fromEntries(subjects.map(subject => [subject, { ...who(subject), sessionId: `${subject}-session`, expiresAt: clock + 3600000 } satisfies SessionIdentity]));
    const mailboxKeyPath = join(dir, 'mailbox.key'), tokensPath = join(dir, 'tokens.json'), signingKeyPath = join(dir, 'signer.pem');
    const config = { domain: 'minarbot.test', accountsHost, dataPath, mailboxKeyPath, identity: { mode: 'stub' as const, tokensPath }, nodeLink: { signingKeyPath } };
    writeFileSync(mailboxKeyPath, Buffer.alloc(32, 8), { mode: 0o600 });
    writeFileSync(tokensPath, JSON.stringify(identities), { mode: 0o600 });
    writeFileSync(signingKeyPath, signer.export({ type: 'pkcs8', format: 'pem' }), { mode: 0o600 });
    const ids = Object.fromEntries(subjects.map(subject => [subject,
        String(JSON.parse(hubCommand(binary, dir, config, 'admit', identities[subject].issuer, subject)).accountId)]));
    const { alice, bob, eve } = ids;
    const hub = await startRustHub(binary, dir, config, 0, true);
    t.after(() => hub.close());
    await hub.control({ now: clock });
    const hubDb = testDatabase(dataPath);
    const a = createHubAuthority(hubDb, { domain: config.domain, mailboxKey: Buffer.alloc(32, 8), now });
    const r = createHubRouting(a);
    const localOrigin = `http://127.0.0.1:${hub.port}`;
    // Test transport dials disposable loopback; production URL/Host and signed origin stay exact.
    const localFetch: typeof fetch = (input, init) => new Promise((resolve, reject) => {
        const url = new URL(String(input));
        const req = httpRequest(localOrigin + url.pathname + url.search, { method: init?.method, headers: Object.fromEntries(new Headers(init?.headers)) }, res => {
            const chunks: Buffer[] = [];
            res.on('data', chunk => chunks.push(Buffer.from(chunk)));
            res.on('end', () => { const headers = new Headers(); for (let i = 0; i < res.rawHeaders.length; i += 2) headers.append(res.rawHeaders[i], res.rawHeaders[i + 1]); resolve(new Response(Buffer.concat(chunks), { status: res.statusCode, headers })); });
        });
        req.on('error', reject); req.end(init?.body);
    });
    let loseEnrollmentResponse = false;
    const hubFetch: typeof fetch = async (input, init) => {
        const response = await localFetch(input, { ...init, headers: { ...init?.headers, Host: accountsHost } });
        if (loseEnrollmentResponse && String(input).endsWith('/enrollment/poll')) { loseEnrollmentResponse = false; throw new Error('Injected response loss'); }
        return response;
    };
    const frames = new EventEmitter();
    let receive: HubLinkOptions['receive'];
    const webSocket = (url: string, settings: import('ws').ClientOptions) => {
        const socket = new WebSocket(url.replace(origin.replace('https', 'wss'), localOrigin.replace('http', 'ws')), { ...settings, headers: { ...settings.headers, Host: accountsHost } });
        socket.on('message', data => frames.emit('frame', String(data)));
        return socket;
    };
    async function browser(host: string, path: string, body?: unknown, session?: { cookies: string; csrf: string }, bearer?: string, expected = 200) {
        const response = await localFetch(localOrigin + path, { method: body === undefined ? 'GET' : 'POST', headers: { Host: host, Origin: `https://${host}`, 'Content-Type': 'application/json', ...(session ? { Cookie: session.cookies, 'X-CSRF-Token': session.csrf } : {}), ...(bearer ? { Authorization: `Bearer ${bearer}` } : {}) }, body: body === undefined ? undefined : JSON.stringify(body) });
        const data = await response.json();
        assert.equal(response.status, expected, JSON.stringify(data));
        return { data, cookies: response.headers.getSetCookie().map(value => value.split(';')[0]).join('; '), csrf: data.csrf };
    }
    async function login(account: string) {
        const subject = account === alice ? 'alice' : account === bob ? 'bob' : 'eve';
        const boot = await browser(accountsHost, '/auth/bootstrap');
        return browser(accountsHost, '/auth/session', {}, boot, subject);
    }
    let providerCalls = 0;
    const provider = createServer(async (req, res) => {
        let raw = '';
        for await (const chunk of req)
            raw += chunk;
        providerCalls++;
        const body = JSON.parse(raw), result = providerScript ? await providerScript(body) : `Reply from ${body.model}`;
        if (res.destroyed)
            return;
        if (result && typeof result === 'object' && 'streamText' in result) {
            res.writeHead(200, { 'Content-Type': 'text/event-stream' });
            res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { content: String(result.streamText) }, finish_reason: null }] })}\n\n`);
            await sleep(Number((result as any).delayMs ?? 300));
            res.end(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] })}\n\ndata: [DONE]\n\n`);
            return;
        }
        res.end(JSON.stringify({ choices: [{ finish_reason: typeof result === 'string' ? 'stop' : 'tool_calls', message: typeof result === 'string' ? { content: result } : { tool_calls: [result] } }] }));
    });
    await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
    const p = provider.address();
    assert(p && typeof p !== 'string');
    const providerPort = p.port;
    const keys = new Map<string, string>(), entry = (service: string, name: string) => ({ getPassword: () => keys.get(`${service}/${name}`) ?? null, setPassword: (value: string) => { keys.set(`${service}/${name}`, value); } }), apps: App[] = [];
    async function node(name: string, account: string, extra: Partial<Parameters<typeof startApp>[0]> = {}) {
        const database = join(dir, `${name}.sqlite`), options = { ...extra, database, stream, baseURL: `http://127.0.0.1:${providerPort}/v1`, hubLink: { origin, hubSigningPublicKey: hubKey, fetch: hubFetch, webSocket, entry, reconnectMs: 10, now, receive: (frame: string, socket: WebSocket) => receive?.(frame, socket) ?? [frame] } };
        let app = await startApp(options);
        apps.push(app);
        const agent = await api(app, '/api/agents', { runtime: 'api', id: randomUUID(), name, model: name, instructions: 'Answer section requests.' }, 201);
        const started = await api(app, '/api/hub-link/enrollment/start', {});
        assert.match(started.humanCode, /^[A-F0-9]{12}$/);
        assert.equal(started.deviceCode, undefined);
        assert.equal(started.privateKey, undefined);
        const session = await login(account);
        await browser(accountsHost, '/api/nodes/enrollment/approve', { humanCode: started.humanCode, fingerprint: '0'.repeat(64) }, undefined, undefined, 401);
        const { data: inspected } = await browser(accountsHost, `/api/nodes/enrollment?humanCode=${started.humanCode}`, undefined, session);
        assert.equal(inspected.nodeId, started.nodeId);
        assert.equal(inspected.fingerprint, started.fingerprint);
        assert.equal(inspected.accountId, account);
        await browser(accountsHost, '/api/nodes/enrollment/approve', { humanCode: started.humanCode, fingerprint: inspected.fingerprint }, session);
        await api(app, '/api/hub-link/enrollment/poll', {});
        await until(() => app.hubLink!.status().connected, 'node online');
        const agentId = randomUUID();
        await browser(accountsHost, '/api/agents', { agentId, nodeId: started.nodeId, name }, session);
        return { get app() { return app; }, agent, agentId, nodeId: String(started.nodeId), database, account, options, async restart() { await app.close().catch(() => {}); app = await startApp(options); apps.push(app); await until(() => app.hubLink!.status().connected, 'reconnected'); }, async participate(invocation: InvocationPolicy = { mode: 'owner' }) { const policy: ParticipationPolicy = { sectionIds: [project.sectionId], invocation, scopes: ['read', 'invoke'], localPolicyDigest: (await api(app, `/api/hub-link/agents/${agent.id}/policy`)).localPolicyDigest, payerBindingRef: 'local_provider', maxCostMicros: 10000 }; const proposal = (await browser(accountsHost, '/api/participations', { projectHost: project.host, agentId, policy, providerMaxCostMicros: 100 }, session)).data;
        await browser(project.host, '/api/participants/consent', proposal, await projectSession(alice)); await api(app, '/api/hub-link/grants/approve', { projectId: project.projectId, participationId: proposal.participationId, agentId, localAgentId: agent.id, revision: proposal.revision, policy, providerMaxCostMicros: 100 }); await until(() => r.grants({ nodeId: String(started.nodeId), accountId: account, nodeGeneration: 1, connectionGeneration: Number(a.one('SELECT connection_generation FROM nodes WHERE id=?', String(started.nodeId))!.connection_generation) }).some(g => g.participationId === proposal.participationId)); return proposal; } };
    }
    t.after(async () => {
        for (const app of apps)
            await app.close().catch(() => { });
        provider.closeAllConnections();
        await new Promise<void>(resolve => provider.close(() => resolve()));
        hubDb.close();
        rmSync(dir, { recursive: true, force: true });
    });
    async function projectSession(account: string) {
        const central = await login(account), started = await browser(project.host, '/auth/start');
        const handoff = await browser(accountsHost, '/auth/handoff', { state: started.data.state }, central);
        const fragment = new URLSearchParams(new URL(handoff.data.callback).hash.slice(1));
        return browser(project.host, '/auth/redeem', { state: fragment.get('state'), code: fragment.get('code') }, { ...started, csrf: fragment.get('state')! });
    }
    const project = (await browser(accountsHost, '/api/projects', { slug: 'alpha', name: 'Alpha' }, await login(alice))).data as { projectId: string; host: string; sectionId: string };
    const ctx = (account: string) => a.resolveProject(project.host, account, project.projectId);
    const { data: invitation } = await browser(project.host, '/api/invitations', { mailbox: 'bob@example.test', role: 'member' }, await projectSession(alice));
    const proof = Object.fromEntries(new URLSearchParams(new URL(invitation.url).hash.slice(1)));
    await browser(project.host, '/api/invitations/accept', proof, await projectSession(bob));
    async function browserPost(account: string, mentions: string[]) {
        const session = await projectSession(account);
        return (await browser(project.host, '/api/messages', { requestId: randomUUID(), sectionId: project.sectionId, text: 'Mention from authenticated project browser', mentions }, session)).data;
    }
    const post = (account: string, mentions: string[]) => r.post(ctx(account), { requestId: randomUUID(), sectionId: project.sectionId, text: 'Explain transport', mentions });
    async function advance(ms: number) {
        await hub.control({ now: clock + ms });
        clock += ms;
    }
    async function queuedPost(input: Parameters<typeof r.post>[1], advanceMs = 0): Promise<ReturnType<typeof r.post>> {
        clock += advanceMs;
        return hub.control({ now: clock, post: { projectId: project.projectId, accountId: alice, input } });
    }
    return { dir, a, r, ctx, alice, bob, eve, project, node, post, browserPost, projectSession, request: localFetch, keys, origin, localOrigin, hubFetch, browser, login, accountsHost, signer, hubKey, frames, loseEnrollmentResponse() { loseEnrollmentResponse = true; }, get calls() { return providerCalls; }, advance, queuedPost, receiveFrames(intercept?: HubLinkOptions['receive']) { receive = intercept; }, async done(turnId: string, account = alice) { await until(() => { const state = r.turn(ctx(account), turnId).state; assert(!['failed', 'unknown', 'revoked'].includes(String(state)), `Unexpected hub turn state: ${state}`); return state === 'done'; }, 'hub final'); return r.history(ctx(account), project.sectionId).results.filter(f => f.turnId === turnId); } };
}
test('signed envelopes reject forged payloads, keys and directions', () => {
    const pair = generateKeyPairSync('ed25519'), key = pair.publicKey.export({ type: 'spki', format: 'der' }).toString('base64url'), body = { kind: 'heartbeat' };
    const envelope = signedNodeMessage(body, pair.privateKey, 'hub');
    assert.deepEqual(verifiedNodeMessage(envelope, key, 'hub'), body);
    for (const value of [{ ...envelope, body: { kind: 'turn.request' } }, { ...envelope, signature: 'x' }, { ...envelope, keyId: '0'.repeat(64) }])
        assert.throws(() => verifiedNodeMessage(value, key, 'hub'));
    assert.throws(() => verifiedNodeMessage(envelope, key, 'node'));
});
test('browser-approved enrollment, two real local engines, attribution and local key custody', async (t) => {
    const f = await fixture(t), alice = await f.node('AliceNode', f.alice), bob = await f.node('BobNode', f.bob), pa = await alice.participate(), pb = await bob.participate({ mode: 'all-members' });
    const post = await f.browserPost(f.alice, [pa.participationId, pb.participationId]);
    for (const turnId of post.turnIds) {
        const results = await f.done(turnId);
        assert.equal(results[0].kind, 'turn.admitted');
        assert.equal(results.at(-1)!.kind, 'turn.final');
        assert(results.some(result => result.kind === 'turn.effect.receipt'));
        assert.equal(results.at(-1)!.requesterAccountId, f.alice);
        assert.equal(results.at(-1)!.payload.text, results.at(-1)!.nodeId === alice.nodeId ? 'Reply from AliceNode' : 'Reply from BobNode');
        assert.equal(results.at(-1)!.payerAccountId, results.at(-1)!.nodeId === alice.nodeId ? f.alice : f.bob);
        assert.deepEqual(results.map(r => r.sequence), results.map((_, i) => i + 1));
    }
    assert.equal(f.calls, 2);
    for (const file of readdirSync(f.dir, { recursive: true, withFileTypes: true })) {
        if (!file.isFile()) continue;
        const filename = join(file.parentPath, file.name), bytes = readFileSync(filename);
        for (const secret of f.keys.values()) {
            assert(!bytes.includes(Buffer.from(secret)), `key leaked into ${filename}`);
            assert(!bytes.includes(Buffer.from(secret, 'base64url')), `raw key leaked into ${filename}`);
        }
    }
    assert.equal(f.keys.size, 6);
});
test('owner-only default, specific-member and all-member grants; offline refuses without hub execution', async (t) => { const f = await fixture(t), bob = await f.node('Bob', f.bob), p = await bob.participate(); assert.throws(() => f.post(f.alice, [p.participationId]), e => e instanceof ProtocolError && e.code === 'authority_denied'); let post = f.post(f.bob, [p.participationId]); await f.done(post.turnIds[0], f.bob); const specific = await bob.participate({ mode: 'members', accountIds: [f.alice] }); post = f.post(f.alice, [specific.participationId]); await f.done(post.turnIds[0]); const all = await bob.participate({ mode: 'all-members' }); post = f.post(f.alice, [all.participationId]); await f.done(post.turnIds[0]); await bob.app.close(); await until(() => f.a.one('SELECT status FROM nodes WHERE id=?', bob.nodeId)!.status === 'offline'); assert.throws(() => f.post(f.alice, [all.participationId]), e => e instanceof ProtocolError && e.code === 'node_offline'); assert.equal(f.calls, 3); assert.throws(() => f.post(f.eve, []), e => e instanceof ProtocolError); });
test('dangerous tool waits for exact local owner approval; hub/browser roles cannot approve it', async (t) => {
    let toolCalls = 0;
    const f = await fixture(t, body => {
        if (body.messages.some((m: any) => m.role === 'tool'))
            return 'Tool finished';
        toolCalls++;
        return { id: 'write-once', type: 'function', function: { name: 'create_text_file', arguments: JSON.stringify({ name: 'result.txt', text: 'Private file created locally.' }) } };
    }), node = await f.node('Writer', f.alice), p = await node.participate(), post = f.post(f.alice, [p.participationId]);
    await until(() => node.app.hubLink!.approvals().length === 1, 'owner approval wait');
    const prompt = node.app.hubLink!.approvals()[0];
    assert.equal(f.calls, 1);
    const before = await api(node.app, `/api/threads/${(await api(node.app, `/api/agents/${node.agent.id}/threads`))[0]?.id}/turns`);
    assert(!before.some((turn: any) => turn.generatedFiles?.length));
    await api(node.app, '/api/hub-link/approvals', { actionId: prompt.actionId, actionDigest: '0'.repeat(64), approved: true }, 403);
    const denied = await fetch(node.app.origin + '/api/hub-link/approvals', { method: 'POST', headers: { Origin: 'https://alpha.minarbot.test', 'Content-Type': 'application/json' }, body: JSON.stringify({ actionId: prompt.actionId, actionDigest: prompt.actionDigest, approved: true }) });
    assert.equal(denied.status, 403);
    await api(node.app, '/api/hub-link/approvals', { actionId: prompt.actionId, actionDigest: prompt.actionDigest, approved: true });
    const results = await f.done(post.turnIds[0]);
    assert(results.some(result => result.kind === 'turn.approval.request' && result.payload.requiredApproverAccountId === f.alice));
    assert(results.some(result => result.kind === 'turn.effect.receipt'));
    assert.equal(results.at(-1)!.payload.text, 'Tool finished');
    assert.equal(toolCalls, 1);
    assert.equal(f.calls, 3);
});
test('duplicate delivery, final ACK loss and restart never execute twice', async (t) => {
    const f = await fixture(t), node = await f.node('Once', f.alice), p = await node.participate();
    let duplicated = false, dropped = false;
    f.receiveFrames((data, socket) => {
        const wire = JSON.parse(data);
        if (wire?.body?.kind === 'turn.request' && !duplicated) {
            duplicated = true;
            return [data, data];
        }
        if (wire.body.kind === 'response' && wire.body.result?.accepted && !dropped) {
            const result = f.a.one('SELECT frame_json FROM node_results WHERE project_id=? AND sequence=?', f.project.projectId, wire.body.result.sequence);
            if (result && JSON.parse(String(result.frame_json)).kind === 'turn.final') {
                dropped = true;
                socket.close(4409, 'Injected ACK loss');
                return [];
            }
        }
        return [data];
    });
    const post = await f.browserPost(f.alice, [p.participationId]);
    await until(() => f.calls === 1);
    await until(() => dropped, 'result ACK dropped');
    assert.equal(f.r.turn(f.ctx(f.alice), post.turnIds[0]).state, 'done');
    await until(() => f.a.one('SELECT connection_generation FROM nodes WHERE id=?', node.nodeId)!.connection_generation !== 1, 'generation fenced');
    await node.restart();
    await sleep(400);
    assert.equal(f.calls, 1);
    assert(duplicated);
    const turn = f.r.turn(f.ctx(f.alice), post.turnIds[0]);
    assert(['done', 'unknown'].includes(String(turn.state)));
});
test('revocation mid-stream stops local work before another effect and does not execute offline', async (t) => { let release: (text: string) => void = () => { }; const f = await fixture(t, () => new Promise<string>(resolve => { release = resolve; })), node = await f.node('Held', f.alice), p = await node.participate(), post = f.post(f.alice, [p.participationId]); await until(() => f.calls === 1); f.a.withdraw(f.ctx(f.alice), p.participationId); await until(async () => { const status = await api(node.app, '/api/hub-link'); return !status.connected || f.r.turn(f.ctx(f.alice), post.turnIds[0]).state === 'unknown'; }); release('Late answer'); await sleep(300); assert.equal(f.calls, 1); assert.equal(f.r.turn(f.ctx(f.alice), post.turnIds[0]).state, 'unknown'); assert.throws(() => f.post(f.alice, [p.participationId]), e => e instanceof ProtocolError); });
test('foreign project and forged signed hub frames never reach local engine', async (t) => {
    const f = await fixture(t), node = await f.node('Isolated', f.alice), p = await node.participate();
    const forged = generateKeyPairSync('ed25519').privateKey;
    for (const scenario of ['foreign project', 'wrong signature']) {
        let injected = false;
        f.receiveFrames(data => {
            const wire = JSON.parse(data);
            if (wire.body.kind !== 'turn.request' || injected) return [data];
            injected = true;
            if (scenario === 'foreign project') {
                const foreign = { ...wire.body, projectId: randomUUID() };
                return [canonicalJson(signedNodeMessage(foreign, f.signer, 'hub'))];
            }
            const { signature } = signedNodeMessage(wire.body, forged, 'hub');
            return [canonicalJson({ ...wire, signature })];
        });
        const post = await f.browserPost(f.alice, [p.participationId]);
        await until(() => injected, scenario);
        await until(() => f.r.turn(f.ctx(f.alice), post.turnIds[0]).state === 'unknown');
        assert.equal(f.calls, 0);
        await node.restart();
    }
});
test('local owner endpoints require exact Origin and refuse bearer callers; unenrolled app stays idle', async (t) => { const f = await fixture(t), node = await f.node('Boundary', f.alice); const r = await fetch(node.app.origin + '/api/hub-link', { headers: { Origin: 'https://foreign.test', Connection: 'close' } }); assert.equal(r.status, 403); const bearer = await fetch(node.app.origin + '/api/hub-link', { headers: { Origin: node.app.origin, Authorization: 'Bearer rejected', Connection: 'close' } }); assert.equal(bearer.status, 403); assert.equal(f.calls, 0); await api(node.app, '/api/hub-link/revoke', {}); await until(() => f.a.one('SELECT status FROM nodes WHERE id=?', node.nodeId)!.status === 'revoked'); assert.equal(node.app.hubLink!.status().disabled, true); });
test('node revocation drains its signed frame before closing transport', async t => {
    const f = await fixture(t), node = await f.node('Delayed revoke', f.alice);
    const original = WebSocket.prototype.send;
    let delayed = false;
    WebSocket.prototype.send = function(data: any, ...args: any[]) {
        const wire = typeof data === 'string' ? JSON.parse(data) : undefined;
        if (wire?.body?.kind === 'node.revoke') {
            delayed = true;
            setTimeout(() => {
                if (this.readyState === WebSocket.OPEN) Reflect.apply(original, this, [data, ...args]);
            }, 50);
            return;
        }
        return Reflect.apply(original, this, [data, ...args]);
    };
    t.after(() => { WebSocket.prototype.send = original; });
    await api(node.app, '/api/hub-link/revoke', {});
    assert(delayed);
    await until(() => f.a.one('SELECT status FROM nodes WHERE id=?', node.nodeId)!.status === 'revoked');
    assert.equal(node.app.hubLink!.status().disabled, true);
    assert.equal(f.calls, 0);
});
test('SIGKILL after dispatch and before result ACK keeps durable uncertainty without replay', async (t) => {
    let release: () => void = () => {};
    const f = await fixture(t, () => new Promise<string>(resolve => { release = () => resolve('Effect already happened'); }));
    const node = await f.node('Crash', f.alice), p = await node.participate();
    const post = await f.browserPost(f.alice, [p.participationId]);
    await until(() => f.calls === 1, 'provider dispatched');
    await node.app.crash();
    release();
    await until(() => f.r.turn(f.ctx(f.alice), post.turnIds[0]).state === 'unknown', 'durable uncertainty');
    await node.restart();
    await sleep(500);
    assert.equal(f.calls, 1);
    assert.equal(f.r.turn(f.ctx(f.alice), post.turnIds[0]).state, 'unknown');
});
test('provider partial snapshots stream over real WebSockets before final', async (t) => { const f = await fixture(t, () => ({ streamText: 'Streamed local answer' }), true), node = await f.node('Stream', f.alice), p = await node.participate(), post = f.post(f.alice, [p.participationId]); const results = await f.done(post.turnIds[0]); assert(results.some(frame => frame.kind === 'turn.partial' && frame.payload.text === 'Streamed local answer')); assert.equal(results.at(-1)!.kind, 'turn.final'); assert.equal(results.at(-1)!.payload.text, 'Streamed local answer'); assert.equal(f.calls, 1); });
test('browser code alone cannot redeem enrollment; node upgrade rejects browser credentials', async (t) => {
    const f = await fixture(t);
    for (const path of ['/api/nodes/enrollment/poll', '/api/nodes/session']) {
        const response = await f.hubFetch(f.origin + path, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ deviceCode: 'BADCODE' }) });
        assert([400, 403].includes(response.status));
    }
    const ws = new WebSocket(f.localOrigin.replace('http', 'ws') + '/node', { headers: { Host: f.accountsHost, Origin: f.origin, Cookie: 'session=forbidden' } });
    const error = await once(ws, 'error');
    assert(error[0] instanceof Error);
});
test('reconnect fences old generation; session expiry renews with node key and never replays completed work', async (t) => {
    const f = await fixture(t), node = await f.node('Renew', f.alice), p = await node.participate();
    const post = await f.browserPost(f.alice, [p.participationId]);
    await f.done(post.turnIds[0]);
    const connectionGeneration = () => Number(f.a.one('SELECT connection_generation FROM nodes WHERE id=?', node.nodeId)!.connection_generation);
    const session = () => f.a.one('SELECT expires_at FROM node_link_sessions WHERE node_id=?', node.nodeId)!.expires_at;
    const before = connectionGeneration(), expiresAt = session();
    await f.advance(900001);
    await until(() => connectionGeneration() > before && node.app.hubLink!.status().connected, 'signed renewal');
    assert(Number(session()) > Number(expiresAt));
    assert.equal(f.calls, 1);
    assert.equal(f.r.turn(f.ctx(f.alice), post.turnIds[0]).state, 'done');
});
test('local provider spend ceiling refuses next request before dispatch', async (t) => {
    const f = await fixture(t, body => body.messages.some((m: any) => m.role === 'tool') ? 'Second paid call' : ({ id: 'budget-tool', type: 'function', function: { name: 'create_text_file', arguments: JSON.stringify({ name: 'budget.txt', text: 'One local file' }) } })), node = await f.node('Budget', f.alice);
    const policy: ParticipationPolicy = { sectionIds: [f.project.sectionId], invocation: { mode: 'owner' }, scopes: ['read', 'invoke'], localPolicyDigest: (await api(node.app, `/api/hub-link/agents/${node.agent.id}/policy`)).localPolicyDigest, payerBindingRef: 'local_provider', maxCostMicros: 100 };
    const p = f.a.propose(f.ctx(f.alice), node.agentId, policy);
    f.a.consent(f.ctx(f.alice), p.participationId, p.revision, p.policyDigest, 'project');
    await api(node.app, '/api/hub-link/grants/approve', { projectId: f.project.projectId, participationId: p.participationId, agentId: node.agentId, localAgentId: node.agent.id, revision: p.revision, policy, providerMaxCostMicros: 100 });
    const post = f.post(f.alice, [p.participationId]);
    await until(() => node.app.hubLink!.approvals().length === 1);
    const prompt = node.app.hubLink!.approvals()[0];
    await api(node.app, '/api/hub-link/approvals', { actionId: prompt.actionId, actionDigest: prompt.actionDigest, approved: true });
    await until(() => f.r.turn(f.ctx(f.alice), post.turnIds[0]).state === 'failed');
    assert.equal(f.calls, 1);
});
test('lost enrollment redemption response recovers with signed node proof', async (t) => {
    const f = await fixture(t);
    f.loseEnrollmentResponse();
    const node = await f.node('RecoveredEnrollment', f.alice);
    assert.equal(node.app.hubLink!.status().enrolled, true);
    assert.equal(f.calls, 0);
});
test('changed local agent policy rejects an existing hub grant before provider dispatch', async (t) => {
    const f = await fixture(t), node = await f.node('Changed', f.alice), p = await node.participate();
    await api(node.app, `/api/agents/${node.agent.id}/edit`, { id: randomUUID(), revision: node.agent.revision, name: 'Changed config', model: 'Changed', instructions: 'Updated policy' });
    const post = f.post(f.alice, [p.participationId]);
    await until(() => f.r.turn(f.ctx(f.alice), post.turnIds[0]).state === 'unknown');
    assert.equal(f.calls, 0);
});

test('revoking alpha preserves simultaneous beta work on the same node',async t=>{
  const held:((value:string)=>void)[]=[];
  const f=await fixture(t,()=>new Promise<string>(resolve=>held.push(resolve))),node=await f.node('TwoProjects',f.alice),alpha=await node.participate();
  const beta=f.a.createProject(f.alice,'beta','Beta'),betaCtx=f.a.resolveProject(beta.host,f.alice,beta.projectId);
  const policy:ParticipationPolicy={sectionIds:[beta.sectionId],invocation:{mode:'owner'},scopes:['read','invoke'],localPolicyDigest:(await api(node.app,`/api/hub-link/agents/${node.agent.id}/policy`)).localPolicyDigest,payerBindingRef:'local_provider',maxCostMicros:10_000};
  const bp=f.a.propose(betaCtx,node.agentId,policy);f.a.consent(betaCtx,bp.participationId,bp.revision,bp.policyDigest,'project');
  await api(node.app,'/api/hub-link/grants/approve',{projectId:beta.projectId,participationId:bp.participationId,agentId:node.agentId,localAgentId:node.agent.id,revision:bp.revision,policy,providerMaxCostMicros:100});
  const a=f.post(f.alice,[alpha.participationId]);await until(()=>held.length===1);
  const b=f.r.post(betaCtx,{requestId:randomUUID(),sectionId:beta.sectionId,text:'Beta work',mentions:[bp.participationId]});await until(()=>held.length===2);
  const generation=f.a.one('SELECT connection_generation FROM nodes WHERE id=?',node.nodeId)!.connection_generation;
  // Revoke when Alpha's result is already in flight, before the hub accepts it.
  const send=WebSocket.prototype.send;
  WebSocket.prototype.send=function(data:unknown,...args:unknown[]){
    const message=typeof data==='string'?JSON.parse(data).body:undefined;
    if(message?.kind==='turn.result'&&message.frame.kind==='turn.final'&&message.frame.turnId===a.turnIds[0]){
      f.a.withdraw(f.ctx(f.alice),alpha.participationId);
    }
    return Reflect.apply(send,this,[data,...args]);
  };
  try {
    held[0]('Late alpha');
    await until(()=>f.r.turn(f.ctx(f.alice),a.turnIds[0]).state==='unknown','Alpha revoked during result delivery');
    held[1]('Beta stays connected');
    await until(()=>{
      const state=f.r.turn(betaCtx,b.turnIds[0]).state;
      assert(['admitted','done'].includes(String(state)),`Unexpected Beta turn state: ${state}`);
      return state==='done';
    },'Beta result after Alpha revocation');
  } finally {
    WebSocket.prototype.send=send;
  }
  assert(!f.r.history(f.ctx(f.alice),f.project.sectionId).results.some(frame=>frame.kind==='turn.final'));
  assert.equal(f.a.one('SELECT connection_generation FROM nodes WHERE id=?',node.nodeId)!.connection_generation,generation);assert.equal(node.app.hubLink!.status().connected,true);assert.equal(f.calls,2);
});

test('offline local withdrawal survives restart and reaches hub before reconnect admits work', async t => {
    const f = await fixture(t), node = await f.node('WithdrawOffline', f.alice), p = await node.participate();
    await node.app.hubLink!.close();
    await until(() => f.a.one('SELECT status FROM nodes WHERE id=?', node.nodeId)!.status === 'offline');
    await api(node.app, '/api/hub-link/grants/revoke', { projectId: f.project.projectId, participationId: p.participationId });
    await node.restart();
    await until(() => f.a.one('SELECT state FROM participations WHERE id=?', p.participationId)!.state === 'revoked', 'durable withdrawal');
    assert.throws(() => f.post(f.alice, [p.participationId]), e => e instanceof ProtocolError);
    assert.equal(f.calls, 0);
});

test('hub-side withdrawal waits offline beyond turn deadline and disables local grant before new work', async t => {
    const f = await fixture(t), node = await f.node('HubWithdrawOffline', f.bob), p = await node.participate();
    await node.app.close();
    await until(() => f.a.one('SELECT status FROM nodes WHERE id=?', node.nodeId)!.status === 'offline');
    await f.browser(f.project.host, '/api/participants/withdraw', { participationId: p.participationId }, await f.projectSession(f.alice));
    await f.advance(600001);
    await node.restart();
    await until(() => f.a.one("SELECT acked FROM outbox WHERE participation_id=? AND kind='grant.revoked'", p.participationId)?.acked === 1, 'durable control ACK');
    await node.app.close();
    const db = testDatabase(node.database);
    try { assert.equal(db.prepare('SELECT enabled FROM hub_link_grants WHERE participation_id=?').get(p.participationId)!.enabled, 0); } finally { db.close(); }
    assert.equal(f.calls, 0);
});

test('withdrawal ACK loss retries same revision and cannot withdraw another node or newer consent', async t => {
    const f = await fixture(t), node = await f.node('WithdrawReceipt', f.alice), other = await f.node('OtherOwner', f.bob), p = await node.participate(), foreign = await other.participate();
    const before = Number(f.a.one('SELECT connection_generation FROM nodes WHERE id=?', node.nodeId)!.connection_generation);
    const original = WebSocket.prototype.send;
    let dropped = false;
    WebSocket.prototype.send = function(data: any, ...args: any[]) {
        const wire = typeof data === 'string' ? JSON.parse(data) : undefined;
        if (!dropped && wire?.body?.kind === 'participation.withdraw') {
            dropped = true;
            const result = Reflect.apply(original, this, [data, ...args]);
            this.close(4409, 'Injected withdrawal ACK loss');
            return result;
        }
        return Reflect.apply(original, this, [data, ...args]);
    };
    t.after(() => { WebSocket.prototype.send = original; });
    await api(node.app, '/api/hub-link/grants/revoke', { projectId: f.project.projectId, participationId: p.participationId });
    await until(() => node.app.hubLink!.status().connected && node.app.hubLink!.status().pendingWithdrawals === 0 && Number(f.a.one('SELECT connection_generation FROM nodes WHERE id=?', node.nodeId)!.connection_generation) > before);
    assert(dropped);
    assert.equal(f.a.one('SELECT revision FROM participations WHERE id=?', p.participationId)!.revision, p.revision + 1);
    const principal = { nodeId: node.nodeId, accountId: f.alice, nodeGeneration: 1, connectionGeneration: Number(f.a.one('SELECT connection_generation FROM nodes WHERE id=?', node.nodeId)!.connection_generation) };
    assert.throws(() => f.a.withdrawNode(principal, f.project.projectId, foreign.participationId, foreign.revision), e => e instanceof ProtocolError && e.code === 'authority_denied');
    const renewed = await node.participate();
    f.a.withdrawNode(principal, f.project.projectId, renewed.participationId, p.revision);
    assert.equal(f.a.one('SELECT state FROM participations WHERE id=?', renewed.participationId)!.state, 'active');
    assert.equal(f.calls, 0);
});

test('accounts enrollment routes reject project cookies, bad CSRF, fingerprint mismatch and account overrides', async t => {
    const f = await fixture(t), node = await f.node('EnrollmentSession', f.alice);
    // A fresh candidate has no approving account until authenticated browser approval.
    const options = { ...node.options, database: join(f.dir, 'candidate.sqlite') }, candidate = await startApp(options);
    t.after(() => candidate.close());
    const started = await api(candidate, '/api/hub-link/enrollment/start', {}), session = await f.login(f.alice);
    const path = '/api/nodes/enrollment/approve', inspected = await f.browser(f.accountsHost, `/api/nodes/enrollment?humanCode=${started.humanCode}`, undefined, session);
    const value = { humanCode: started.humanCode, fingerprint: inspected.data.fingerprint };
    await f.browser(f.accountsHost, path, value, { ...session, csrf: 'bad' }, undefined, 403);
    await f.browser(f.accountsHost, path, { ...value, fingerprint: '0'.repeat(64) }, session, undefined, 404);
    await f.browser(f.accountsHost, path, { ...value, accountId: f.bob }, session, undefined, 400);
    const project = await f.projectSession(f.alice);
    await f.browser(f.accountsHost, path, value, project, undefined, 401);
    await f.browser(f.project.host, path, value, project, undefined, 404);
    const crossOrigin = await f.request(f.origin + path, { method: 'POST', headers: { Host: f.accountsHost, Origin: 'https://alpha.minarbot.test', Cookie: session.cookies, 'X-CSRF-Token': session.csrf, 'Content-Type': 'application/json' }, body: JSON.stringify(value) });
    assert.equal(crossOrigin.status, 403);
    await f.browser(f.accountsHost, path, value, session);
    const bob = await f.login(f.bob);
    await f.browser(f.accountsHost, path, value, bob, undefined, 403);
    await f.browser(f.accountsHost, '/auth/logout', { global: false }, session);
    await f.browser(f.accountsHost, path, value, session, undefined, 401);
    await api(candidate, '/api/hub-link/enrollment/poll', {});
    assert.equal(candidate.hubLink!.status().accountId, f.alice);
});

test('computer input requires local approval and action lease; revoke leaves uncertain input without replay', async t => {
    let effects = 0, release: () => void = () => {}, hold = false;
    const runtime = {
        launch() {
            const child = Object.assign(new EventEmitter(), { exitCode: null, stdin: { end() { queueMicrotask(() => { Object.defineProperty(child, 'exitCode', { value: 0 }); child.emit('exit', 0); }); } } }) as unknown as ChildProcess;
            return { child, ready: Promise.resolve() };
        },
        async run() { effects++; if (hold) await new Promise<void>(resolve => { release = resolve; }); return Buffer.alloc(0); },
        async capture() { return Buffer.alloc(0); },
    };
    const call = { id: 'hub-computer-click', type: 'function', function: { name: 'computer_click', arguments: '{"x":12,"y":24}' } };
    const f = await fixture(t, body => body.messages.some((m: any) => m.role === 'tool') ? 'Computer checked.' : call);
    const node = await f.node('ComputerBoundary', f.alice, { desktop: { dataDir: join(f.dir, 'computer'), leaseDir: join(f.dir, 'leases'), enabled: true, runtime } });
    await api(node.app, `/api/agents/${node.agent.id}/computer`, { id: randomUUID(), revision: 0, enabled: true });
    const p = await node.participate(), first = await f.browserPost(f.alice, [p.participationId]);
    await until(() => node.app.hubLink!.approvals().length === 1, 'computer owner approval');
    assert.equal(effects, 0);
    const approval = node.app.hubLink!.approvals()[0];
    assert.equal((approval.payload as any).function.name, 'computer_click');
    await api(node.app, '/api/hub-link/approvals', { actionId: approval.actionId, actionDigest: approval.actionDigest, approved: true });
    const results = await f.done(first.turnIds[0]);
    assert.equal(effects, 1);
    assert(results.some(result => result.kind === 'turn.effect.receipt' && result.payload.actionId === approval.actionId && result.payload.outcome === 'succeeded'));
    assert(f.a.one('SELECT 1 FROM action_leases WHERE action_id=?', approval.actionId));
    hold = true;
    const second = f.post(f.alice, [p.participationId]);
    await until(() => node.app.hubLink!.approvals().length === 1);
    const pending = node.app.hubLink!.approvals()[0];
    await api(node.app, '/api/hub-link/approvals', { actionId: pending.actionId, actionDigest: pending.actionDigest, approved: true });
    await until(() => effects === 2, 'computer effect dispatched');
    f.a.withdraw(f.ctx(f.alice), p.participationId);
    await until(() => node.app.hubLink!.approvals().length === 0 && f.r.turn(f.ctx(f.alice), second.turnIds[0]).state === 'unknown');
    release();
    await node.restart();
    await sleep(200);
    assert.equal(effects, 2);
    await node.app.close();
    const db = testDatabase(node.database);
    try { assert.equal(db.prepare('SELECT state FROM hub_link_actions WHERE action_id=?').get(pending.actionId)!.state, 'unknown'); } finally { db.close(); }
    assert.throws(() => f.post(f.alice, [p.participationId]), e => e instanceof ProtocolError);
});

test('connected app calls require local approval and lease; revocation after dispatch is uncertain and never replayed', async t => {
    const { composioToolName } = await import('./test-support/composio-tool-name.ts');
    let effects = 0, lose = false, release: () => void = () => {};
    const remote = createServer(async (req, res) => {
        let raw = ''; for await (const chunk of req) raw += chunk;
        const path = new URL(req.url!, 'http://fixture').pathname;
        res.setHeader('Content-Type', 'application/json');
        if (path.endsWith('/toolkits/categories')) { res.end('{"items":[]}'); return; }
        if (path.endsWith('/connected_accounts')) { res.end(JSON.stringify({ items: [{ id: 'fixture-account', toolkit: { slug: 'gmail' }, status: 'ACTIVE' }] })); return; }
        if (path.endsWith('/tools')) { res.end(JSON.stringify({ items: [{ slug: 'GMAIL_SEND_EMAIL', name: 'Send email', description: 'Synthetic write', toolkit: { slug: 'gmail' }, version: '20261113_00', input_parameters: { type: 'object', properties: { target: { type: 'string' } }, required: ['target'], additionalProperties: false }, tags: ['createHint'] }] })); return; }
        if (path.endsWith('/tools/execute/GMAIL_SEND_EMAIL')) {
            effects++;
            if (lose) { await new Promise<void>(resolve => { release = resolve; }); res.destroy(); return; }
            res.end('{"successful":true,"data":{"fixture":"sent"}}'); return;
        }
        res.writeHead(404); res.end('{}');
    });
    remote.listen(0, '127.0.0.1'); await once(remote, 'listening');
    const address = remote.address(); assert(address && typeof address !== 'string');
    t.after(async () => { release(); remote.closeAllConnections(); await new Promise<void>(resolve => remote.close(() => resolve())); });
    const tool = { id: 'connected-write', type: 'function', function: { name: composioToolName('GMAIL_SEND_EMAIL'), arguments: '{"target":"synthetic@example.test"}' } };
    const f = await fixture(t, body => body.messages.some((m: any) => m.role === 'tool') ? 'Connected app completed.' : tool);
    const node = await f.node('ConnectedBoundary', f.alice, { composio: { baseURL: `http://127.0.0.1:${address.port}/api/v3.1` } });
    await api(node.app, '/api/connected-apps/key', { id: randomUUID(), revision: 0, key: 'synthetic-composio-fixture-only' });
    await api(node.app, `/api/agents/${node.agent.id}/connected-apps/gmail`, { id: randomUUID(), revision: 0, enabled: true });
    const p = await node.participate(), first = f.post(f.alice, [p.participationId]);
    await until(() => node.app.hubLink!.approvals().length === 1, 'hub tool approval');
    const approval = node.app.hubLink!.approvals()[0]; assert.equal(effects, 0);
    await api(node.app, '/api/hub-link/approvals', { actionId: approval.actionId, actionDigest: approval.actionDigest, approved: true });
    // Local app approval remains separate from hub action approval.
    let local: any;
    const [thread] = await api(node.app, `/api/agents/${node.agent.id}/threads`);
    const turnsPath = `/api/threads/${thread.id}/turns`;
    await until(async () => { local = (await api(node.app, turnsPath)).find((row: any) => row.id === first.turnIds[0]); return local?.approvals?.some((row: any) => row.state === 'approval'); }, 'connected app approval');
    assert.equal(effects, 0);
    const appApproval = local.approvals[0];
    await api(node.app, `${turnsPath}/${first.turnIds[0]}/app-approvals/${appApproval.id}`, { id: randomUUID(), revision: appApproval.revision, decision: 'allow' });
    await until(() => node.app.hubLink!.approvals().length === 1, 'fresh resume lease approval');
    const resume = node.app.hubLink!.approvals()[0];
    await api(node.app, '/api/hub-link/approvals', { actionId: resume.actionId, actionDigest: resume.actionDigest, approved: true });
    await f.done(first.turnIds[0]); assert.equal(effects, 1);
    assert(f.a.one('SELECT 1 FROM action_leases WHERE action_id=?', resume.actionId));
    lose = true;
    const second = f.post(f.alice, [p.participationId]);
    await until(() => node.app.hubLink!.approvals().length === 1);
    const next = node.app.hubLink!.approvals()[0];
    await api(node.app, '/api/hub-link/approvals', { actionId: next.actionId, actionDigest: next.actionDigest, approved: true });
    await until(async () => { local = (await api(node.app, turnsPath)).find((row: any) => row.id === second.turnIds[0]); return local?.approvals?.some((row: any) => row.state === 'approval'); });
    const secondApproval = local.approvals[0];
    await api(node.app, `${turnsPath}/${second.turnIds[0]}/app-approvals/${secondApproval.id}`, { id: randomUUID(), revision: secondApproval.revision, decision: 'allow' });
    await until(() => node.app.hubLink!.approvals().length === 1);
    const nextResume = node.app.hubLink!.approvals()[0];
    await api(node.app, '/api/hub-link/approvals', { actionId: nextResume.actionId, actionDigest: nextResume.actionDigest, approved: true });
    await until(() => effects === 2);
    f.a.withdraw(f.ctx(f.alice), p.participationId); release();
    await until(() => f.r.turn(f.ctx(f.alice), second.turnIds[0]).state === 'unknown');
    await node.restart(); await sleep(200); assert.equal(effects, 2);
    assert.throws(() => f.post(f.alice, [p.participationId]), e => e instanceof ProtocolError);
});

test('MCP async discovery cannot dispatch after revoke; lost result remains uncertain across restart', async t => {
    let connection = '', effects = 0, holdCatalogue = false, held = false, loseResult = false, release: () => void = () => {};
    const remote = createServer(async (req, res) => {
        if (req.method === 'GET') { res.writeHead(405); res.end(); return; }
        if (req.method === 'DELETE') { res.end(); return; }
        let raw = ''; for await (const chunk of req) raw += chunk;
        const message = JSON.parse(raw);
        if (message.id === undefined) { res.writeHead(202); res.end(); return; }
        let result: unknown;
        if (message.method === 'initialize') result = { protocolVersion: '2025-03-26', capabilities: { tools: {} }, serverInfo: { name: 'hub-effect-fixture', version: '1' } };
        else if (message.method === 'tools/list') {
            if (holdCatalogue) { held = true; await new Promise<void>(resolve => { release = resolve; }); }
            result = { tools: [{ name: 'change', inputSchema: { type: 'object', properties: { value: { type: 'string' } }, required: ['value'] } }] };
        } else if (message.method === 'tools/call') {
            effects++;
            if (loseResult) { res.destroy(); return; }
            result = { content: [{ type: 'text', text: 'Saved synthetic change.' }] };
        } else { res.writeHead(400); res.end(); return; }
        res.setHeader('Content-Type', 'application/json');
        res.end(JSON.stringify({ jsonrpc: '2.0', id: message.id, result }));
    });
    remote.listen(0, '127.0.0.1'); await once(remote, 'listening');
    const address = remote.address(); assert(address && typeof address !== 'string');
    t.after(async () => { release(); remote.closeAllConnections(); await new Promise<void>(resolve => remote.close(() => resolve())); });
    const f = await fixture(t, body => body.messages.some((m: any) => m.role === 'tool') ? 'MCP completed.' : ({ id: 'mcp-write', type: 'function', function: { name: 'call_mcp_tool', arguments: JSON.stringify({ connection, name: 'change', arguments: { value: 'synthetic' } }) } }));
    const node = await f.node('McpBoundary', f.alice);
    const tested = await api(node.app, '/api/mcp/test', { id: randomUUID(), name: 'Fixture', url: `http://127.0.0.1:${address.port}/mcp`, token: '' });
    connection = randomUUID(); await api(node.app, '/api/mcp', { id: connection, test: tested.id });
    await api(node.app, `/api/agents/${node.agent.id}/mcp/${connection}`, { id: randomUUID(), revision: 0, connectionRevision: 1, tools: ['change'] });
    const p = await node.participate();
    const approve = async () => {
        await until(() => node.app.hubLink!.approvals().length === 1);
        const approval = node.app.hubLink!.approvals()[0];
        await api(node.app, '/api/hub-link/approvals', { actionId: approval.actionId, actionDigest: approval.actionDigest, approved: true });
        return approval;
    };
    const first = f.post(f.alice, [p.participationId]); await approve(); await f.done(first.turnIds[0]); assert.equal(effects, 1);
    holdCatalogue = true;
    const second = f.post(f.alice, [p.participationId]); await approve(); await until(() => held);
    f.a.withdraw(f.ctx(f.alice), p.participationId);
    await until(() => f.r.turn(f.ctx(f.alice), second.turnIds[0]).state === 'unknown');
    // Wait for node's local cancellation, not just synchronous hub state change.
    await until(() => { try { node.app.hubLink!.check(second.turnIds[0]); return false; } catch { return true; } });
    release(); holdCatalogue = false; await sleep(100); assert.equal(effects, 1);
    await node.restart();
    const renewed = await node.participate(); loseResult = true;
    const third = f.post(f.alice, [renewed.participationId]), action = await approve();
    await until(() => f.r.turn(f.ctx(f.alice), third.turnIds[0]).state === 'failed');
    assert.equal(effects, 2);
    const receipts = f.r.history(f.ctx(f.alice), f.project.sectionId).results;
    assert(receipts.some(row => row.kind === 'turn.effect.receipt' && row.payload.actionId === action.actionId && row.payload.outcome === 'unknown'));
    await node.restart(); await sleep(150); assert.equal(effects, 2);
});

for (const runtime of ['codex', 'claude', 'hermes', 'antigravity', 'grok', 'pi', 'opencode', 'gemini', 'kiro']) test(`${runtime}: unsupported effect fencing never executes hub turns`, async t => {
    const f = await fixture(t), node = await f.node('NativeBoundary', f.alice);
    const workspace = join(f.dir, 'native-workspace'); mkdirSync(workspace);
    Object.assign(node.options, { workspaces: [{ agent: node.agent.id, directory: workspace, write: false }], harness: { executables: { [runtime]: process.execPath }, issuer: { mintAgentSession() { assert.fail('Unsupported native hub turn minted an execution session.'); }, revokeAgentSession() {} } } });
    await node.app.close();
    const db = testDatabase(node.database);
    try { db.prepare('UPDATE agents SET runtime=? WHERE id=?').run(runtime, node.agent.id); } finally { db.close(); }
    await node.restart();
    const status = await api(node.app, '/api/hub-link');
    assert.deepEqual(status.supportedRuntimes, ['api']);
    assert.equal(status.unsupportedRuntimes[runtime], 'native_effect_fencing_unavailable');
    const p = await node.participate(), post = f.post(f.alice, [p.participationId]);
    await until(() => f.r.turn(f.ctx(f.alice), post.turnIds[0]).state === 'failed');
    const results = f.r.history(f.ctx(f.alice), f.project.sectionId).results.filter(row => row.turnId === post.turnIds[0]);
    assert.equal(results.at(-1)!.payload.code, 'local_engine_unsupported_or_unavailable');
    assert.equal(f.calls, 0);
    await node.restart(); assert.equal(f.calls, 0);
});

test('two session-enrolled nodes answer browser mention; revoking one live stream preserves other node', async t => {
    const f = await fixture(t, body => ({ streamText: `Partial from ${body.model}`, delayMs: body.model === 'HeldStream' ? 2000 : 300 }), true);
    const alice = await f.node('HeldStream', f.alice), bob = await f.node('OtherStream', f.bob);
    const pa = await alice.participate(), pb = await bob.participate({ mode: 'all-members' });
    const post = await f.browserPost(f.alice, [pa.participationId, pb.participationId]);
    const revokedTurn = post.turnIds.find((id: string) => f.r.turn(f.ctx(f.alice), id).node_id === alice.nodeId);
    const otherTurn = post.turnIds.find((id: string) => id !== revokedTurn);
    await until(() => f.r.history(f.ctx(f.alice), f.project.sectionId).results.some(row => row.turnId === revokedTurn && row.kind === 'turn.partial'), 'live partial before revoke');
    f.a.withdraw(f.ctx(f.alice), pa.participationId);
    await until(() => { try { alice.app.hubLink!.check(revokedTurn); return false; } catch { return true; } });
    const results = await f.done(otherTurn);
    assert.equal(results.at(-1)!.nodeId, bob.nodeId);
    assert.equal(results.at(-1)!.payload.text, 'Partial from OtherStream');
    assert.equal(f.r.turn(f.ctx(f.alice), revokedTurn).state, 'unknown');
    assert(!f.r.history(f.ctx(f.alice), f.project.sectionId).results.some(row => row.turnId === revokedTurn && row.kind === 'turn.final'));
    assert.equal(f.calls, 2);
});

test('offline withdrawal is acknowledged even after owner loses project membership', async t => {
    const f = await fixture(t), node = await f.node('RemovedOwner', f.bob), p = await node.participate();
    await node.app.hubLink!.close();
    await api(node.app, '/api/hub-link/grants/revoke', { projectId: f.project.projectId, participationId: p.participationId });
    f.a.editMember(f.ctx(f.alice), f.bob, 1, 'revoked');
    await node.restart();
    assert.equal(node.app.hubLink!.status().pendingWithdrawals, 0);
    assert.equal(f.a.one('SELECT state FROM participations WHERE id=?', p.participationId)!.state, 'revoked');
    assert.equal(f.calls, 0);
});

test('project queue admission is atomic, idempotent and expires without executing offline work', async t => {
    const f = await fixture(t), node = await f.node('QueueBoundary', f.alice), p = await node.participate();
    const input = { requestId: randomUUID(), sectionId: f.project.sectionId, text: 'Bounded queue', mentions: [p.participationId] };
    // Let the hosted queue fill while withholding delivery to the local engine.
    f.receiveFrames(frame => ['section.event', 'turn.request'].includes(JSON.parse(frame).body.kind) ? [] : [frame]);
    const first = await f.queuedPost(input);
    for (let i = 1; i < 100; i++) await f.queuedPost({ ...input, requestId: randomUUID() });
    await assert.rejects(f.queuedPost({ ...input, requestId: randomUUID() }), e => e instanceof ProtocolError && e.status === 429 && e.code === 'project_queue_full');
    assert.deepEqual(await f.queuedPost(input), first);
    assert.equal(f.a.one('SELECT count(*) AS n FROM turns WHERE project_id=?', f.project.projectId)!.n, 100);
    assert.equal((await f.queuedPost({ ...input, requestId: randomUUID() }, 600001)).turnIds.length, 1);
    await node.restart();
    f.receiveFrames();
    assert.equal(f.calls, 0);
});

test('connected app resume requires exactly one ready action before issuing any lease', async t => {
    let release: (value: string) => void = () => {};
    const f = await fixture(t, () => new Promise<string>(resolve => { release = resolve; })), node = await f.node('BatchBoundary', f.alice), p = await node.participate();
    const post = f.post(f.alice, [p.participationId]);
    await until(() => f.calls === 1);
    await assert.rejects(node.app.hubLink!.beforeEffect(post.turnIds[0], 'tool', { kind: 'composio.resume' }), e => e instanceof ProtocolError && e.code === 'hub_effect_batch_unsupported');
    assert.equal(node.app.hubLink!.approvals().length, 0);
    release('No connected app effects.'); await f.done(post.turnIds[0]); assert.equal(f.calls, 1);
});

test('human post rate leaves capacity for withdrawal controls', async t => {
    const f = await fixture(t), node = await f.node('PostRate', f.alice), p = await node.participate(), session = await f.projectSession(f.alice);
    for (let i = 0; i < 60; i++) await f.browser(f.project.host, '/api/messages', { requestId: randomUUID(), sectionId: f.project.sectionId, text: 'Bounded human post', mentions: [] }, session);
    await f.browser(f.project.host, '/api/messages', { requestId: randomUUID(), sectionId: f.project.sectionId, text: 'Over limit', mentions: [] }, session, undefined, 429);
    await api(node.app, '/api/hub-link/grants/revoke', { projectId: f.project.projectId, participationId: p.participationId });
    assert.equal(f.a.one('SELECT state FROM participations WHERE id=?', p.participationId)!.state, 'revoked');
    assert.equal(f.calls, 0);
});

test('withdrawal drains its control frame before queued section delivery closes transport', async t => {
    const f = await fixture(t), node = await f.node('Queued withdrawal', f.alice), p = await node.participate();
    const original = WebSocket.prototype.send;
    let delayed = false;
    WebSocket.prototype.send = function(data: any, ...args: any[]) {
        const wire = typeof data === 'string' ? JSON.parse(data) : undefined;
        if (wire?.body?.kind === 'participation.withdraw') {
            delayed = true;
            f.post(f.alice, []);
            setTimeout(() => {
                if (this.readyState === WebSocket.OPEN) Reflect.apply(original, this, [data, ...args]);
            }, 500);
            return;
        }
        return Reflect.apply(original, this, [data, ...args]);
    };
    t.after(() => { WebSocket.prototype.send = original; });
    await api(node.app, '/api/hub-link/grants/revoke', { projectId: f.project.projectId, participationId: p.participationId });
    assert(delayed);
    assert.equal(f.a.one('SELECT state FROM participations WHERE id=?', p.participationId)!.state, 'revoked');
    assert.equal(f.calls, 0);
});

test('signed pending requests stay node-scoped; approval, stale revision, offline decline and reconnect preserve consent', async t => {
    const f = await fixture(t), bob = await f.node('Pending Bob', f.bob), alice = await f.node('Other Alice', f.alice), web = createHubWebApi(f.a);
    web.registerAgent(f.bob, { agentId: bob.agent.id, nodeId: bob.nodeId, name: 'Bob local' });
    const policy: ParticipationPolicy = { sectionIds: [f.project.sectionId], scopes: ['read','invoke'], invocation: { mode: 'all-members' }, payerBindingRef: 'local', localPolicyDigest: bob.app.hubLink!.localPolicyDigest(bob.agent.id), maxCostMicros: 10000 };
    const propose = () => web.propose(f.ctx(f.bob), { agentId: bob.agent.id, policy, providerMaxCostMicros: 100 });
    assert.throws(() => web.propose(f.ctx(f.bob), { agentId: bob.agent.id, policy, providerMaxCostMicros: 10001 }));
    const first = propose();
    await until(() => bob.app.hubLink!.requests().length === 1, 'pending request delivered');
    const pending = bob.app.hubLink!.requests()[0];
    assert.equal(pending.projectName, 'Alpha'); assert.equal(pending.requesterAccountId, f.bob);
    assert.equal(pending.sections[0].id, f.project.sectionId); assert.equal(pending.grant.providerMaxCostMicros, 100);
    assert.equal(alice.app.hubLink!.requests().length, 0); assert.equal(f.calls, 0);
    const changed = propose(); assert(changed.revision > first.revision);
    await until(() => bob.app.hubLink!.requests()[0]?.grant.revision === changed.revision, 'new revision delivered');
    await api(bob.app, '/api/hub-link/grants/approve', pending.grant, 409);
    const current = bob.app.hubLink!.requests()[0].grant;
    await api(bob.app, '/api/hub-link/grants/approve', current);
    assert.equal(f.a.one('SELECT state FROM participations WHERE id=?', changed.participationId)!.state, 'proposed');
    assert.equal(bob.app.hubLink!.requests().length, 0); assert.equal(f.calls, 0);
    f.a.consent(f.ctx(f.alice), changed.participationId, changed.revision, changed.policyDigest, 'project');
    assert.equal(f.a.one('SELECT state FROM participations WHERE id=?', changed.participationId)!.state, 'active');
    const declined = propose();
    await until(() => bob.app.hubLink!.requests()[0]?.grant.revision === declined.revision);
    await bob.app.hubLink!.close();
    await api(bob.app, '/api/hub-link/grants/revoke', { projectId: f.project.projectId, participationId: declined.participationId });
    assert.equal(bob.app.hubLink!.status().pendingWithdrawals, 1); assert.equal(bob.app.hubLink!.requests().length, 0);
    await bob.restart();
    await until(() => bob.app.hubLink!.status().pendingWithdrawals === 0, 'offline decline reconciled');
    assert.equal(f.a.one('SELECT state FROM participations WHERE id=?', declined.participationId)!.state, 'revoked');
    assert.equal(bob.app.hubLink!.requests().length, 0); assert.equal(f.calls, 0);
    let chunked = false;
    f.frames.on('frame', (frame: string) => {
        const body = JSON.parse(frame).body;
        if (body?.kind === 'participation.requests') {
            assert(Buffer.byteLength(frame) <= 65536);
            if (!body.complete) chunked = true;
        }
    });
    for (let index = 0; index < 90; index++) {
        const agentId = randomUUID(); web.registerAgent(f.bob, { agentId, nodeId: bob.nodeId, name: `Pending ${index}` });
        web.propose(f.ctx(f.bob), { agentId, policy, providerMaxCostMicros: 100 });
    }
    await until(() => bob.app.hubLink!.requests().length === 90, 'bounded multi-frame snapshot');
    assert(chunked); assert.equal(bob.app.hubLink!.status().connected, true);
});

test('local owner deletes linked agent, withdraws hub participation and removes local transport journals', async t => {
    const f = await fixture(t), node = await f.node('Delete linked', f.alice), p = await node.participate();
    await api(node.app, '/api/agents', { runtime: 'api', id: randomUUID(), name: 'Keep', model: 'fixture', instructions: 'Help' }, 201);
    const post = f.post(f.alice, [p.participationId]); await f.done(post.turnIds[0]);
    await api(node.app, `/api/agents/${node.agent.id}/delete`, { id: randomUUID() });
    assert(!(await api(node.app, '/api/agents')).agents.some((agent: any) => agent.id === node.agent.id));
    assert.deepEqual(await api(node.app, '/api/hub-link/history'), []);
    await node.app.close();
    const db = testDatabase(node.database);
    try {
        for (const table of ['hub_link_threads','hub_link_grants','hub_link_turns','hub_link_events','hub_link_results','hub_link_actions']) assert.equal(db.prepare(`SELECT count(*) AS n FROM ${table}`).get()!.n, 0, table);
        assert.deepEqual(db.prepare('PRAGMA foreign_key_check').all(), []);
    } finally { db.close(); }
});

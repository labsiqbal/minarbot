import type { ChildProcess } from 'node:child_process';
import { once, EventEmitter } from 'node:events';
import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer, request as httpRequest } from 'node:http';
import { generateKeyPairSync, randomUUID, createPublicKey } from 'node:crypto';
import { mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { markMinarbotData, testDatabase } from './test-support/database.ts';
import { hubCommand, startRustHub, type PostInput, type SessionIdentity } from './test-support/hosted-hub.ts';
import { rustBinary } from './rust-binary.ts';
import type { HubLinkOptions } from './rust-hub-test-bridge.ts';
import WebSocket from 'ws';
import { startApp } from './test-bridge.ts';
import { signedNodeMessage, verifiedNodeMessage } from './test-support/hub-node-signature.ts';
import { canonicalJson, ProtocolError, sha256 } from './shared/protocol.ts';
type ParticipationPolicy = { sectionIds: string[]; invocation: { mode: 'owner' | 'all-members' } | { mode: 'members'; accountIds: string[] }; scopes: ('read' | 'invoke')[]; localPolicyDigest: string; payerBindingRef: string; maxCostMicros: number };
type InvocationPolicy = ParticipationPolicy['invocation'];
type RoutingFrame = {
    kind: string; turnId: string; nodeId: string; sequence: number;
    requesterAccountId: string; payerAccountId: string; payload: Record<string, unknown>;
};
type Project = { projectId: string; host: string; sectionId: string };
type Proposal = { participationId: string; revision: number; policyDigest: string };
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
    writeFileSync(dataPath, '', { mode: 0o600 });
    const marker = testDatabase(dataPath);
    markMinarbotData(marker);
    marker.close();
    const ids = Object.fromEntries(subjects.map(subject => [subject,
        String(JSON.parse(hubCommand(binary, dir, config, 'admit', identities[subject].issuer, subject)).accountId)]));
    const { alice, bob, eve } = ids;
    const hub = await startRustHub(binary, dir, config, 0, true);
    t.after(() => hub.close());
    await hub.control({ now: clock });
    const hubDb = testDatabase(dataPath, { readOnly: true });
    const one = (sql: string, ...args: (string | number)[]) => hubDb.prepare(sql).get(...args);
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
    async function browser(host: string, path: string, body?: unknown, session?: { cookies: string; csrf: string }, bearer?: string, expected?: number) {
        const response = await localFetch(localOrigin + path, { method: body === undefined ? 'GET' : 'POST', headers: { Host: host, Origin: `https://${host}`, 'Content-Type': 'application/json', ...(session ? { Cookie: session.cookies, 'X-CSRF-Token': session.csrf } : {}), ...(bearer ? { Authorization: `Bearer ${bearer}` } : {}) }, body: body === undefined ? undefined : JSON.stringify(body) });
        const data = await response.json();
        if (expected === undefined && !response.ok) throw new ProtocolError(response.status, data.error.code, data.error.message);
        assert.equal(response.status, expected ?? 200, JSON.stringify(data));
        return { data, cookies: response.headers.getSetCookie().map(value => value.split(';')[0]).join('; '), csrf: data.csrf };
    }
    async function login(account: string) {
        const subject = account === alice ? 'alice' : account === bob ? 'bob' : 'eve';
        const boot = await browser(accountsHost, '/auth/bootstrap');
        return browser(accountsHost, '/auth/session', {}, boot, subject);
    }
    const sessions = new Map<string, Awaited<ReturnType<typeof login>>>();
    async function centralSession(account: string) {
        const prior = sessions.get(account);
        if (prior && prior.data.expiresAt > clock) return prior;
        const session = await login(account);
        sessions.set(account, session);
        return session;
    }
    async function createProject(account: string, slug: string, name: string): Promise<Project> {
        return (await browser(accountsHost, '/api/projects', { slug, name }, await centralSession(account))).data;
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
        const session = await centralSession(account);
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
        return {
            get app() { return app; }, agent, agentId, nodeId: String(started.nodeId), database, account, options,
            async restart() {
                await app.close().catch(() => {});
                app = await startApp(options);
                apps.push(app);
                await until(() => app.hubLink!.status().connected, 'reconnected');
            },
            async participate(invocation: InvocationPolicy = { mode: 'owner' }, target = project) {
                const policy: ParticipationPolicy = {
                    sectionIds: [target.sectionId], invocation, scopes: ['read', 'invoke'],
                    localPolicyDigest: (await api(app, `/api/hub-link/agents/${agent.id}/policy`)).localPolicyDigest,
                    payerBindingRef: 'local_provider', maxCostMicros: 10000,
                };
                const proposal = await propose(account, agentId, policy, 100, target);
                await consent(proposal, target);
                await api(app, '/api/hub-link/grants/approve', {
                    projectId: target.projectId, participationId: proposal.participationId, agentId,
                    localAgentId: agent.id, revision: proposal.revision, policy, providerMaxCostMicros: 100,
                });
                await until(() => one("SELECT 1 FROM participations WHERE id=? AND state='active'", proposal.participationId), 'grant active');
                return proposal;
            },
        };
    }
    t.after(async () => {
        for (const app of apps)
            await app.close().catch(() => { });
        provider.closeAllConnections();
        await new Promise<void>(resolve => provider.close(() => resolve()));
        hubDb.close();
        rmSync(dir, { recursive: true, force: true });
    });
    async function projectSession(account: string, target = project) {
        const central = await centralSession(account), started = await browser(target.host, '/auth/start');
        const handoff = await browser(accountsHost, '/auth/handoff', { state: started.data.state }, central);
        const fragment = new URLSearchParams(new URL(handoff.data.callback).hash.slice(1));
        return browser(target.host, '/auth/redeem', { state: fragment.get('state'), code: fragment.get('code') }, { ...started, csrf: fragment.get('state')! });
    }
    const project = await createProject(alice, 'alpha', 'Alpha');
    const { data: invitation } = await browser(project.host, '/api/invitations', { mailbox: 'bob@example.test', role: 'member' }, await projectSession(alice));
    const proof = Object.fromEntries(new URLSearchParams(new URL(invitation.url).hash.slice(1)));
    await browser(project.host, '/api/invitations/accept', proof, await projectSession(bob));
    async function post(account: string, mentions: string[], target = project, text = 'Explain transport') {
        const session = await projectSession(account, target);
        return (await browser(target.host, '/api/messages', { requestId: randomUUID(), sectionId: target.sectionId, text, mentions }, session)).data as { turnIds: string[] };
    }
    const browserPost = (account: string, mentions: string[]) =>
        post(account, mentions, project, 'Mention from authenticated project browser');
    const registerAgent = async (account: string, agentId: string, nodeId: string, name: string) =>
        browser(accountsHost, '/api/agents', { agentId, nodeId, name }, await centralSession(account));
    async function propose(account: string, agentId: string, policy: ParticipationPolicy, providerMaxCostMicros = 100, target = project): Promise<Proposal> {
        return (await browser(accountsHost, '/api/participations', { projectHost: target.host, agentId, policy, providerMaxCostMicros }, await centralSession(account))).data;
    }
    const consent = async (proposal: Proposal, target = project) =>
        browser(target.host, '/api/participants/consent', proposal, await projectSession(alice, target));
    const withdraw = async (participationId: string) =>
        browser(project.host, '/api/participants/withdraw', { participationId }, await projectSession(alice));
    async function advance(ms: number) {
        await hub.control({ now: clock + ms });
        clock += ms;
    }
    async function queuedPost(input: PostInput, advanceMs = 0): Promise<{ turnIds: string[] }> {
        clock += advanceMs;
        return hub.control({ now: clock, post: { projectId: project.projectId, accountId: alice, input } });
    }
    const turn = (turnId: string, target = project) =>
        one('SELECT * FROM turns WHERE project_id=? AND id=?', target.projectId, turnId)!;
    const results = (): RoutingFrame[] => hubDb.prepare(`SELECT r.frame_json FROM node_results r
        JOIN turns t ON t.project_id=r.project_id AND t.id=r.turn_id WHERE t.project_id=? AND t.section_id=? ORDER BY t.rowid,r.sequence`)
        .all(project.projectId, project.sectionId).map(row => JSON.parse(String(row.frame_json)));
    const withdrawNode = (principal: { nodeId: string; accountId: string; nodeGeneration: number; connectionGeneration: number }, participationId: string, revision: number) =>
        hub.control({ now: clock, withdraw_node: { principal, input: { projectId: project.projectId, participationId, revision } } });
    return {
        dir, one, turn, results, alice, bob, eve, project, createProject, node, post, browserPost, projectSession, request: localFetch,
        keys, origin, localOrigin, hubFetch, browser, login, accountsHost, signer, hubKey, frames, advance, queuedPost,
        registerAgent, propose, consent, withdraw, withdrawNode,
        loseEnrollmentResponse() { loseEnrollmentResponse = true; }, get calls() { return providerCalls; },
        receiveFrames(intercept?: HubLinkOptions['receive']) { receive = intercept; },
        async done(turnId: string, account = alice) {
            const session = await projectSession(account);
            await until(async () => {
                const { data } = await browser(project.host, `/api/turn?turnId=${turnId}`, undefined, session);
                assert(!['failed', 'unknown', 'revoked'].includes(data.state), `Unexpected hub turn state: ${data.state}`);
                return data.state === 'done';
            }, 'hub final');
            const { data } = await browser(project.host, `/api/history?sectionId=${project.sectionId}`, undefined, session);
            return (data.results as RoutingFrame[]).filter(frame => frame.turnId === turnId);
        },
    };
}

test('signed envelopes reject forged payloads, keys and directions', () => {
    const pair = generateKeyPairSync('ed25519'), key = pair.publicKey.export({ type: 'spki', format: 'der' }).toString('base64url'), body = { kind: 'heartbeat' };
    const envelope = signedNodeMessage(body, pair.privateKey, 'hub');
    assert.deepEqual(verifiedNodeMessage(envelope, key, 'hub'), body);
    for (const value of [{ ...envelope, body: { kind: 'turn.request' } }, { ...envelope, signature: 'x' }, { ...envelope, keyId: '0'.repeat(64) }])
        assert.throws(() => verifiedNodeMessage(value, key, 'hub'));
    assert.throws(() => verifiedNodeMessage(envelope, key, 'node'));
});
// Kept for open product gap P6 (docs/test-ledger.md): native harness turns reserve the hub
// provider budget once per harness pass, so the second paid call is not refused.
test('local provider spend ceiling refuses next request before dispatch', async (t) => {
    const f = await fixture(t, body => body.messages.some((m: any) => m.role === 'tool') ? 'Second paid call' : ({ id: 'budget-tool', type: 'function', function: { name: 'create_text_file', arguments: JSON.stringify({ name: 'budget.txt', text: 'One local file' }) } })), node = await f.node('Budget', f.alice);
    const policy: ParticipationPolicy = { sectionIds: [f.project.sectionId], invocation: { mode: 'owner' }, scopes: ['read', 'invoke'], localPolicyDigest: (await api(node.app, `/api/hub-link/agents/${node.agent.id}/policy`)).localPolicyDigest, payerBindingRef: 'local_provider', maxCostMicros: 100 };
    const p = await f.propose(f.alice, node.agentId, policy);
    await f.consent(p);
    await api(node.app, '/api/hub-link/grants/approve', { projectId: f.project.projectId, participationId: p.participationId, agentId: node.agentId, localAgentId: node.agent.id, revision: p.revision, policy, providerMaxCostMicros: 100 });
    const post = await f.post(f.alice, [p.participationId]);
    await until(() => node.app.hubLink!.approvals().length === 1);
    const prompt = node.app.hubLink!.approvals()[0];
    await api(node.app, '/api/hub-link/approvals', { actionId: prompt.actionId, actionDigest: prompt.actionDigest, approved: true });
    await until(() => f.turn(post.turnIds[0]).state === 'failed');
    assert.equal(f.calls, 1);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer, type Server } from 'node:http';
import { createHash, randomBytes, randomUUID } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync, existsSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { testDatabase } from './test-support/database.ts';
import { startRustServer } from './rust-test-server.ts';
import { startApp } from './test-bridge.ts';
import { runRustConnector, runRustConnectorJournal } from './rust-connectors-runner.ts';
import { composioToolName } from './test-support/composio-tool-name.ts';

test('Rust Composio approval recovery, revocation, schema checks and uncertain writes remain fenced', async () => {
  const root = mkdtempSync(join(tmpdir(), 'minarbot-rust-composio-')), database = join(root, 'db');
  const key = 'FIXTURE_COMPOSIO_KEY_93', agent = randomUUID(), responses: string[] = [], effects: unknown[] = [];
  let mode = 'read', disconnected = false, reachedUpstream!: () => void;
  const upstreamAccepted = new Promise<void>(resolve => { reachedUpstream = resolve; });
  const remote = createServer(async (request, response) => {
    assert.equal(request.headers['x-api-key'], key);
    let raw = ''; for await (const part of request) raw += part;
    const path = new URL(request.url!, 'http://fixture').pathname;
    if (path.endsWith('/toolkits') || path.endsWith('/toolkits/categories')) { response.end(JSON.stringify({ items: [] })); return; }
    if (path.endsWith('/connected_accounts')) { response.end(JSON.stringify({ items: [{ id: 'ca_fixture', toolkit: { slug: 'gmail' }, status: 'ACTIVE' }] })); return; }
    if (path.endsWith('/tools')) {
      response.end(JSON.stringify({ items: ['READ', 'WRITE'].map(kind => ({ slug: `GMAIL_${kind}`, name: kind, description: key, toolkit: { slug: 'gmail' }, version: 'fixture', tags: [kind === 'READ' ? 'readOnlyHint' : 'createHint'], input_parameters: { type: 'object', properties: { target: { type: 'string' } }, required: ['target'], additionalProperties: false } })) })); return;
    }
    effects.push(JSON.parse(raw));
    if (mode === 'crash') { reachedUpstream(); return; }
    if (mode === 'uncertain') { disconnected = true; request.socket.destroy(); return; }
    response.end(JSON.stringify({ successful: true, data: { result: key } }));
  });
  const provider = createServer(async (request, response) => {
    let raw = ''; for await (const part of request) raw += part;
    assert(!raw.includes(key));
    const body = JSON.parse(raw), result = body.messages.findLast((message: any) => message.role === 'tool');
    const message = result ? { content: result.content } : { tool_calls: [{ id: 'fixture-call', type: 'function', function: { name: composioToolName(mode === 'read' ? 'GMAIL_READ' : 'GMAIL_WRITE'), arguments: JSON.stringify(mode === 'invalid' ? { unexpected: true } : { target: 'fixture@example.test' }) } }] };
    response.end(JSON.stringify({ choices: [{ finish_reason: result ? 'stop' : 'tool_calls', message }] }));
  });
  const options = { database, composio: { baseURL: await listen(remote) + '/api/v3.1' }, baseURL: await listen(provider) + '/v1' };
  let app = await startApp(options, { directory: root, servers: [remote, provider] });
  const request = async (path: string, body?: unknown, status = 200): Promise<any> => {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const raw = await response.text(); responses.push(raw); assert.equal(response.status, status, raw); return JSON.parse(raw);
  };
  const turns = `/api/agents/${agent}/turns`, grant = `/api/agents/${agent}/connected-apps/gmail`;
  const settled = async (id: string, predicate: (row: any) => boolean) => {
    for (let i = 0; i < 500; i++) {
      const row = (await request(turns)).find((item: any) => item.id === id);
      if (row && predicate(row)) return row;
      await new Promise(resolve => setTimeout(resolve, 20));
    }
    assert.fail(`Turn ${id} did not settle`);
  };
  try {
    await request('/api/agents', { id: agent, runtime: 'api', name: 'Fixture', model: 'fixture', instructions: 'Help' }, 201);
    await request('/api/connected-apps/key', { id: randomUUID(), revision: 0, key });
    await request(grant, { id: randomUUID(), revision: 0, enabled: true });
    const run = async () => { const id = randomUUID(); await request(turns, { id, text: mode }, 202); return id; };
    const read = await run(); await settled(read, row => row.status === 'done'); assert.equal(effects.length, 1);
    mode = 'write'; const write = await run(), approval = (await settled(write, row => row.approvals?.some((item: any) => item.state === 'approval'))).approvals[0];
    await app.close(); app = await startRustServer(options);
    const path = `${turns}/${write}/app-approvals/${approval.id}`, decision = { id: randomUUID(), revision: approval.revision, decision: 'allow' };
    await request(path, { ...decision, revision: 999 }, 409); await request(path, decision); await request(path, decision);
    await settled(write, row => row.status === 'done'); assert.equal(effects.length, 2);
    const revoke = await run(), pending = (await settled(revoke, row => row.approvals?.some((item: any) => item.state === 'approval'))).approvals[0];
    await request(grant, { id: randomUUID(), revision: 1, enabled: false });
    await request(`${turns}/${revoke}/app-approvals/${pending.id}`, { id: randomUUID(), revision: pending.revision, decision: 'allow' }, 409);
    await request(`${turns}/${revoke}/app-approvals/${pending.id}`, { id: randomUUID(), revision: pending.revision, decision: 'deny' });
    await settled(revoke, row => row.status === 'done'); assert.equal(effects.length, 2);
    await request(grant, { id: randomUUID(), revision: 2, enabled: true });
    mode = 'invalid'; const invalid = await run(); await settled(invalid, row => row.status === 'done' || row.status === 'failed'); assert.equal(effects.length, 2);
    mode = 'uncertain'; const uncertain = await run(), last = (await settled(uncertain, row => row.approvals?.some((item: any) => item.state === 'approval'))).approvals[0];
    await request(`${turns}/${uncertain}/app-approvals/${last.id}`, { id: randomUUID(), revision: last.revision, decision: 'allow' });
    await settled(uncertain, row => row.approvals?.some((item: any) => item.state === 'uncertain')); assert(disconnected); assert.equal(effects.length, 3);
    await app.close(); app = await startRustServer(options); assert.equal(effects.length, 3);
    mode = 'crash';
    const killed = await run();
    const beforeCrash = (await settled(killed, row => row.approvals?.some((item: any) => item.state === 'approval'))).approvals[0];
    await request(`${turns}/${killed}/app-approvals/${beforeCrash.id}`, { id: randomUUID(), revision: beforeCrash.revision, decision: 'allow' });
    let crashDeadline: ReturnType<typeof setTimeout> | undefined;
    try {
      await Promise.race([upstreamAccepted, new Promise<never>((_, reject) => {
        crashDeadline = setTimeout(() => reject(new Error('Composio fixture never accepted the pending write.')), 5000);
      })]);
    } finally { clearTimeout(crashDeadline); }
    const dispatched = testDatabase(database);
    try { assert.equal(dispatched.prepare('SELECT state FROM composio_calls WHERE turn=?').get(killed)!.state, 'running'); }
    finally { dispatched.close(); }
    await app.crash();
    app = await startRustServer(options);
    const crash = await settled(killed, row => row.status === 'interrupted');
    assert.equal(crash.approvals[0].state, 'uncertain');
    await request(`${turns}/${killed}/resume`, { id: randomUUID(), revision: crash.revision, acknowledgeUncertain: true });
    const failed = await settled(killed, row => row.status === 'failed');
    assert.match(failed.error, /uncertain/);
    assert.equal(effects.length, 4, 'Acknowledging uncertainty must never send the write again.');
    assert(!responses.join('').includes(key)); assert(!readFileSync(database).includes(Buffer.from(key)));
  } finally { await app.close(); await Promise.all([close(remote), close(provider)]); rmSync(root, { recursive: true, force: true }); }
});
for (const platform of ['slack', 'telegram']) test(`Rust ${platform} offline recovery preserves confirmed delivery and requires confirmation`, async () => {
  const root = mkdtempSync(join(tmpdir(), 'minarbot-rust-journal-')), directory = join(root, 'state'), id = randomUUID();
  const command = (command: string, extra = {}) => runRustConnectorJournal({ directory, platform, command, ...extra });
  try {
    const initial = await command('status'); assert.deepEqual(initial.inbox, []); assert.deepEqual(initial.outbox, []);
    const db = testDatabase(join(directory, `${platform}.sqlite`));
    try {
      if (platform === 'slack') db.prepare("INSERT INTO slack_outbox(id,channel,sender,root,text,status,part,timestamps) VALUES(?,'DCHAT','UUSER','1700000000.000001','Exact output','uncertain',1,'[\"1700000001.000001\"]')").run(id);
      else db.prepare("INSERT INTO telegram_outbox(id,chat,reply_to,text,status,part,message_ids,error) VALUES(?,4,1,'Exact output','uncertain',1,'[42]','Lost response')").run(id);
      const original = (await command('inspect', { id })).reply;
      await assert.rejects(command('recover', { id, revision: 0, action: 'retry' }), /confirmation/);
      assert.deepEqual((await command('inspect', { id })).reply, original);
      assert.deepEqual(await command('recover', { id, revision: 0, action: 'retry', confirmation: '--accept-duplicate-risk' }), { id, status: 'queued', revision: 1 });
      const retry = await command('inspect', { id }); assert.equal(retry.reply.part, 1); assert.equal(retry.reply.text, 'Exact output'); assert.equal(retry.decisions[0].status, 'uncertain');
      assert.equal(retry.reply[platform === 'slack' ? 'timestamps' : 'message_ids'], original[platform === 'slack' ? 'timestamps' : 'message_ids']);
      await assert.rejects(command('recover', { id, revision: 0, action: 'retry', confirmation: '--accept-duplicate-risk' }), /changed/);
      db.prepare(`UPDATE ${platform}_outbox SET status='failed' WHERE id=?`).run(id);
      assert.deepEqual(await command('recover', { id, revision: 1, action: 'discard', confirmation: '--confirm' }), { id, status: 'discarded', revision: 2 });
      assert.equal((await command('status')).outbox[0].revision, 2);
    } finally { db.close(); }
    assert(!existsSync(join(directory, 'owner.lock')));
  } finally { rmSync(root, { recursive: true, force: true }); }
});
async function listen(server: Server) {
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
  const address = server.address(); assert(address && typeof address !== 'string');
  return `http://127.0.0.1:${address.port}`;
}
async function close(server: Server) {
  server.closeAllConnections(); await new Promise<void>(resolve => server.close(() => resolve()));
}
async function wait(condition: () => boolean) {
  for (let i = 0; i < 500; i++) { if (condition()) return; await new Promise(resolve => setTimeout(resolve, 20)); }
  assert(condition(), 'Fixture condition timed out.');
}
function frame(value: unknown) {
  const bytes = Buffer.from(JSON.stringify(value));
  const head = bytes.length < 126 ? Buffer.from([0x81, bytes.length]) : Buffer.from([0x81, 126, bytes.length >> 8, bytes.length & 255]);
  return Buffer.concat([head, bytes]);
}

for (const platform of ['telegram', 'slack'] as const) for (const mode of ['reply', 'question', 'file', 'workflow', 'media']) {
  const question = mode === 'question', file = mode === 'file', workflow = mode === 'workflow', media = mode === 'media';
  test(`Rust ${platform} runner ${mode} through native turns`, async () => {
    const root = mkdtempSync(join(tmpdir(), 'minarbot-rust-connector-')), agent = randomUUID(), controller = new AbortController();
    const sockets = new Set<import('node:stream').Duplex>();
    let calls = 0, deliveries = 0, answer: string | undefined, offset = 0, acknowledgements = 0, connections = 0;
    const requests: string[] = [];
    const workflowId = randomUUID(); let workflowRun: any, uploaded = 0, downloaded = 0;
    const provider = createServer(async (req, res) => {
      let raw = ''; for await (const part of req) raw += part;
      const body = JSON.parse(raw); calls++;
      const tool = (question || file) && !body.messages.some((m: { role: string }) => m.role === 'tool');
      res.end(JSON.stringify({ choices: [{ finish_reason: tool ? 'tool_calls' : 'stop', message: tool ? { tool_calls: [{ id: 'effect', type: 'function', function: { name: file ? 'create_text_file' : 'ask_user', arguments: JSON.stringify(file ? { name: 'fixture.txt', text: 'Exact native file bytes' } : { question: 'Proceed?', choices: ['Yes', 'No'] }) } }] } : { content: 'Native connector reply' } }] }));
    });
    const providerOrigin = await listen(provider);
    const app = await startApp({ database: join(root, 'app.sqlite'), baseURL: providerOrigin + '/v1' }, { directory: root, servers: [provider] });
    const ui = async (path: string, body?: unknown): Promise<any> => {
      const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
      const value = await response.json(); assert(response.ok, JSON.stringify(value)); return value;
    };
    const token = randomBytes(32).toString('base64url');
    const local = createServer(async (req, res) => {
      if (req.headers.authorization !== `Bearer ${token}`) { res.writeHead(403); res.end(); return; }
      requests.push(req.url!);
      let raw = ''; for await (const part of req) raw += part;
      const base = `/api/scripts/agents/${agent}`;
      if (workflow && req.url!.includes('/workflow')) {
        const body = raw ? JSON.parse(raw) : undefined;
        if (req.url!.endsWith('/run')) { assert.equal(body.revision, 1); assert.equal(body.input, 'Exact input'); workflowRun = { id: body.id, status: 'approval', revision: 1, prompt: 'Proceed?', generatedFiles: [] }; }
        else if (req.url!.endsWith('/approve')) { assert.equal(body.revision, 1); assert.equal(body.accepted, true); assert.notEqual(body.id, workflowRun.id); workflowRun = { ...workflowRun, status: 'done', revision: 2, result: 'Exact input' }; }
        res.end(JSON.stringify(workflowRun)); return;
      }
      const path = (req.url!.startsWith(base + '/threads/') ? req.url!.slice(base.length).replace(/^\/threads\//, '/api/threads/') : req.url!.replace(base, `/api/agents/${agent}`)).replace(/\/turns\/[^/]+\/files\//, '/files/');
      if (media && req.headers['content-type'] === 'application/octet-stream') { assert.equal(raw, 'Native media bytes'); uploaded++; }
      const response = await fetch(app.origin + path, { method: req.method, headers: { Origin: app.origin, 'Content-Type': String(req.headers['content-type'] ?? 'application/json'), ...(req.headers['x-file-name'] ? { 'X-File-Name': String(req.headers['x-file-name']) } : {}) }, body: raw || undefined });
      res.writeHead(response.status, { 'Content-Type': response.headers.get('content-type') ?? 'application/json' });
      res.end(Buffer.from(await response.arrayBuffer()));
    });
    const localOrigin = await listen(local);
    const directory = join(root, 'scripts'); mkdirSync(directory, { mode: 0o700 });
    writeFileSync(join(directory, 'connection.json'), JSON.stringify({ url: localOrigin, token }), { mode: 0o600 });
    const event = (id: number, text = workflow ? `!workflow ${workflowId} 1 Exact input` : 'Native request') => ({ type: 'events_api', envelope_id: `e${id}`, payload: { type: 'event_callback', team_id: 'TTEAM', api_app_id: 'AAPP', event_id: `Ev${id}`, authorizations: [{ team_id: 'TTEAM', user_id: 'UBOT', is_bot: true }], event: { type: 'message', channel_type: 'im', channel: 'DCHAT', user: 'UUSER', text, ...(media ? { files: [{ id: 'FTEXT' }], subtype: 'file_share' } : {}), ts: `1700000000.${String(id).padStart(6, '0')}`, ...(id > 1 ? { thread_ts: '1700000000.000001' } : {}) } } });
    const remote = createServer(async (req, res) => {
      let raw = ''; for await (const part of req) raw += part;
      if (req.url === '/download' || req.url!.startsWith('/file/')) {
        if (platform === 'slack') assert.equal(req.headers.authorization, 'Bearer xoxb-' + 'b'.repeat(24));
        downloaded++; res.end('Native media bytes'); return;
      }
      if (req.url === '/upload' || req.url!.endsWith('/sendDocument')) {
        assert(raw.includes('Exact native file bytes')); deliveries++;
        res.end(JSON.stringify({ ok: true, result: { message_id: 40, chat: { id: 4 } } }));
        if (platform === 'telegram') setTimeout(() => controller.abort(), 100);
        return;
      }
      const body = JSON.parse(raw); let result: unknown;
      if (platform === 'telegram') {
        if (req.url!.endsWith('/getMe')) result = { id: 123, is_bot: true };
        else if (req.url!.endsWith('/getFile')) result = { file_id: 'FTEXT', file_path: 'fixture.txt', file_size: 18 };
        else if (req.url!.endsWith('/getUpdates')) {
          assert.equal(body.timeout, 25); assert.equal(body.limit, 20); offset = Math.max(offset, body.offset);
          result = body.offset < 8 ? [{ update_id: 7, message: { message_id: 8, chat: { id: 4, type: 'private' }, from: { id: 4, is_bot: false }, text: workflow ? `/workflow ${workflowId} 1 Exact input` : 'Native request', ...(media ? { document: { file_id: 'FTEXT', file_name: 'fixture.txt', file_size: 18 }, caption: 'Native request' } : {}) } }] : answer && body.offset < 9 ? [{ update_id: 8, message: { message_id: 9, chat: { id: 4, type: 'private' }, from: { id: 4, is_bot: false }, text: workflow ? answer : `/answer ${answer} Yes` } }] : [];
        } else {
          assert.equal(body.chat_id, 4); deliveries++; result = { message_id: 20 + deliveries, chat: { id: 4 } };
          if (workflow && String(body.text).includes('/approve ')) answer = /\/approve [a-f0-9-]{36} 1 yes/.exec(body.text)?.[0];
          if (String(body.text).includes('/answer ')) answer = /\/answer ([a-f0-9-]{36})/.exec(body.text)?.[1];
          if (body.text === (workflow ? 'Exact input' : 'Native connector reply') && !file) setTimeout(() => controller.abort(), 100);
        }
        res.end(JSON.stringify({ ok: true, result }));
      } else {
        if (req.url!.endsWith('/auth.test')) result = { team_id: 'TTEAM', user_id: 'UBOT', bot_id: 'BBOT' };
        else if (req.url!.endsWith('/files.info')) { const address = remote.address(); assert(address && typeof address !== 'string'); result = { file: { id: 'FTEXT', name: 'fixture.txt', mode: 'hosted', is_external: false, size: 18, url_private_download: `http://127.0.0.1:${address.port}/download` } }; }
        else if (req.url!.endsWith('/apps.connections.open')) { const address = remote.address(); assert(address && typeof address !== 'string'); result = { url: `ws://127.0.0.1:${address.port}/socket` }; }
        else if (req.url!.endsWith('/files.getUploadURLExternal')) { const address = remote.address(); assert(address && typeof address !== 'string'); result = { file_id: 'FFILE', upload_url: `http://127.0.0.1:${address.port}/upload` }; }
        else if (req.url!.endsWith('/files.completeUploadExternal')) { result = { files: body.files }; setTimeout(() => controller.abort(), 100); }
        else {
          assert.equal(body.channel, 'DCHAT'); assert.equal(body.thread_ts, '1700000000.000001'); deliveries++;
          result = { channel: 'DCHAT', ts: `1700000001.${String(deliveries).padStart(6, '0')}` };
          if (workflow && String(body.text).includes('!approve ')) { answer = /!approve [a-f0-9-]{36} 1 yes/.exec(body.text)?.[0]; for (const socket of sockets) socket.write(frame(event(2, answer))); }
          if (String(body.text).includes('!answer ')) { answer = /!answer ([a-f0-9-]{36})/.exec(body.text)?.[1]; for (const socket of sockets) socket.write(frame(event(2, `!answer ${answer} Yes`))); }
          if (body.text === (workflow ? 'Exact input' : 'Native connector reply') && !file) setTimeout(() => controller.abort(), 100);
        }
        res.end(JSON.stringify({ ok: true, ...result as object }));
      }
    });
    remote.on('upgrade', (req, socket) => {
      connections++; sockets.add(socket); socket.on('close', () => sockets.delete(socket));
      const accept = createHash('sha1').update(String(req.headers['sec-websocket-key']) + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64');
      socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
      socket.write(frame({ type: 'hello', connection_info: { app_id: 'AAPP' } })); socket.write(frame(event(1)));
      if (workflow && answer) socket.write(frame(event(2, answer)));
      let pending = Buffer.alloc(0);
      socket.on('data', chunk => {
        pending = Buffer.concat([pending, chunk]);
        while (pending.length >= 6) {
          const code = pending[0] & 15, size = pending[1] & 127;
          if (size >= 126 || pending.length < 6 + size) return;
          const mask = pending.subarray(2, 6), body = Buffer.from(pending.subarray(6, 6 + size)); pending = pending.subarray(6 + size);
          for (let i = 0; i < body.length; i++) body[i] ^= mask[i % 4];
          if (code === 8) { socket.end(Buffer.from([0x88, 0])); continue; }
          if (code !== 1) continue;
          const ack = JSON.parse(body.toString()); assert(['e1', 'e2'].includes(ack.envelope_id)); acknowledgements++;
          if (connections === 1 && ack.envelope_id === 'e1') socket.write(frame({ type: 'disconnect', reason: 'refresh_requested' }));
        }
      });
    });
    const origin = await listen(remote);
    const timeout = setTimeout(() => controller.abort(), 12000);
    try {
      await ui('/api/agents', { id: agent, runtime: 'api', name: 'Native', model: 'fixture', instructions: 'Help' });
      await runRustConnector(platform, { directory: join(root, 'state'), scriptDirectory: directory, agent, origin, users: platform === 'slack' ? ['UUSER'] : [4], ...(platform === 'slack' ? { team: 'TTEAM', app: 'AAPP', botToken: 'xoxb-' + 'b'.repeat(24), appToken: 'xapp-' + 'a'.repeat(24) } : { token: '123:' + 'x'.repeat(24) }) }, controller.signal);
      assert.equal(calls, workflow ? 0 : file ? 3 : question ? 2 : 1); assert.equal(deliveries, question || workflow ? 3 : file ? 2 : 1);
      if (media) { assert.equal(downloaded, 1); assert.equal(uploaded, 1); }
      if (workflow) assert.equal(workflowRun.status, 'done');
      assert(requests.some(path => path.includes(workflow ? '/workflows/' : '/turns'))); assert(!existsSync(join(root, 'state', 'owner.lock')));
      const journal = testDatabase(join(root, 'state', `${platform}.sqlite`));
      try { assert(journal.prepare(`SELECT status FROM ${platform}_outbox`).all().every(row => row.status === 'sent')); } finally { journal.close(); }
      if (platform === 'slack') { assert(connections >= 2); assert(acknowledgements >= 2); } else assert(offset >= 8);
      assert.deepEqual(await ui(`/api/agents/${agent}/turns`), []);
    } finally { clearTimeout(timeout); controller.abort(); for (const socket of sockets) socket.destroy(); await app.close(); await Promise.all([close(local), close(remote), close(provider)]); rmSync(root, { recursive: true, force: true }); }
  });
}

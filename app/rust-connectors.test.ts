import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer, type Server } from 'node:http';
import { createHash, randomBytes, randomUUID } from 'node:crypto';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync, existsSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { testDatabase } from './test-support/database.ts';
import { startApp } from './test-bridge.ts';
import { runRustConnector } from './rust-connectors-runner.ts';

// Other cases moved to crates/minarbot-e2e/tests/e2e/fx_connectors.rs and
// fx_composio.rs (#89). The question runs stay here for B1: their model call
// count leaves out the step the harness takes after ask_user.
async function listen(server: Server) {
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
  const address = server.address(); assert(address && typeof address !== 'string');
  return `http://127.0.0.1:${address.port}`;
}
async function close(server: Server) {
  server.closeAllConnections(); await new Promise<void>(resolve => server.close(() => resolve()));
}
function frame(value: unknown) {
  const bytes = Buffer.from(JSON.stringify(value));
  const head = bytes.length < 126 ? Buffer.from([0x81, bytes.length]) : Buffer.from([0x81, 126, bytes.length >> 8, bytes.length & 255]);
  return Buffer.concat([head, bytes]);
}

for (const platform of ['telegram', 'slack'] as const) for (const mode of ['question']) {
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

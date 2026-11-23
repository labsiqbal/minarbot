// Disposable app, synthetic streaming provider, Git workspace and paired peer.
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { startApp } from '../../test-bridge.ts';

const data = process.argv[2];
if (!data) throw new Error('data dir required');
mkdirSync(data, { recursive: true });
const releases = join(data, 'releases');
mkdirSync(join(releases, 'feed'), { recursive: true });
writeFileSync(join(releases, 'feed/latest.json'), JSON.stringify({ id: '0.11.0-12345678', version: '0.11.0', commit: '12345678'.repeat(5), created: new Date().toISOString(), node: process.version, notes: 'Synthetic release notes', files: {} }));

const gitDir = join(data, 'coder-repo'); mkdirSync(gitDir, { recursive: true });
const git = (...a) => execFileSync('git', ['-c', 'core.hooksPath=/dev/null', ...a], { cwd: gitDir, env: { PATH: '/usr/bin:/bin', GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: '/dev/null', GIT_TERMINAL_PROMPT: '0' } });
const fresh = !existsSync(join(gitDir, '.git'));
if (fresh) {
git('init', '-q', '--initial-branch=main'); git('config', 'user.name', 'Audit'); git('config', 'user.email', 'audit@example.test');
writeFileSync(join(gitDir, 'README.md'), '# Landing\n\nOriginal intro line.\nShared context\n');
writeFileSync(join(gitDir, 'index.html'), '<!doctype html><title>Landing</title><h1>Landing v1</h1>');
git('add', '.'); git('commit', '-q', '-m', 'Baseline');
writeFileSync(join(gitDir, 'README.md'), '# Landing\n\nUpdated intro line with more detail.\nShared context\nA brand new line.\n');
writeFileSync(join(gitDir, 'index.html'), '<!doctype html><title>Landing</title><h1>Landing v2</h1><p>Synthetic preview body.</p>');
writeFileSync(join(gitDir, 'draft.txt'), 'Untracked draft\n');
}
const idFile = join(data, 'coder-id'); if (!existsSync(idFile)) writeFileSync(idFile, randomUUID());
const coderId = readFileSync(idFile, 'utf8').trim(); mkdirSync(join(data, 'projects'), { recursive: true });
const markdown = `## Release checklist

Here is the plan, with \`inline code\` and a [link](https://example.test).

1. Confirm the build
2. Run the smoke suite
   - desktop at 1440px
   - narrow at 1024px

\`\`\`ts
export function greet(name) {
  return \`Hello, \${name}\`;
}
\`\`\`

| Area | Owner | Status |
| --- | --- | --- |
| Chat | Iris | Done |
| Rooms | Theo | In progress |
| Settings | Mara | Blocked |

> A short quote to check blockquote styling.

That's everything for now.`;
const short = 'Sure - here is a short synthetic reply. Anything else?';
const slow = Array.from({ length: 50 }, (_, i) => `Paragraph ${i + 1}: a long synthetic answer that keeps streaming so Stop, steer and queue can be exercised.\n\n`).join('');

async function streamText(res, text, step = 24, ms = 35, tail) {
  res.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache' });
  const emit = (delta, finish_reason = null) => res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta, finish_reason }] })}\n\n`);
  for (let i = 0; i < text.length; i += step) { if (res.destroyed) return; emit({ content: text.slice(i, i + step) }); await delay(ms); }
  if (tail) { emit({ tool_calls: [tail] }); emit({}, 'tool_calls'); }
  else emit({}, 'stop');
  res.end('data: [DONE]\n\n');
}

const provider = createServer(async (req, res) => {
  try {
    let raw = ''; for await (const chunk of req) raw += chunk;
    const body = raw ? JSON.parse(raw) : {};
    const json = (status, value) => { res.writeHead(status, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(value)); };
    if (req.method === 'GET' && req.url === '/v1/models') return json(200, { object: 'list', data: ['audit-model', 'audit-fast'].map(id => ({ id, object: 'model' })) });
    const messages = body.messages ?? [];
    const lastUser = String([...messages].reverse().find(m => m.role === 'user')?.content ?? '');
    const system = String(messages[0]?.content ?? '');
    const toolDone = messages.some(m => m.role === 'tool');
    let text = short, tail;
    const room = system.includes('Group chat.');
    if (room) {
      const own = system.match(/Your member ID: ([^.]+)\./)?.[1];
      const name = own && system.match(new RegExp(`"id":"${own}","name":"([^"]+)`))?.[1];
      const turnNo = (system.match(/Round (\d) of 3/) ?? [])[1];
      text = turnNo === '1' || !turnNo ? `${name}: from my side, the main risk is scope. I suggest we ship the smallest slice first and review together.` : 'NO_REPLY';
    } else if (lastUser === 'long slow UXA stream') {
      res.writeHead(200, { 'Content-Type': 'text/event-stream' });
      res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { content: 'Synthetic active reply.' }, finish_reason: null }] })}\n\n`);
      await Promise.race([delay(120000), new Promise(resolve => res.once('close', resolve))]);
      if (!res.destroyed) res.end(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] })}\n\ndata: [DONE]\n\n`);
      return;
    } else if (/fail/i.test(lastUser)) return json(400, { error: { message: 'Synthetic provider failure' } });
    else if (/slow|long/i.test(lastUser)) text = slow;
    else if (/delay/i.test(lastUser)) await delay(4000);
    else if (/hold tool/i.test(lastUser) && !toolDone) { text = 'Saving the draft first.'; tail = { index: 0, id: randomUUID(), type: 'function', function: { name: 'create_text_file', arguments: JSON.stringify({ name: 'held.txt', text: 'Held step' }) } }; }
    else if (/hold tool/i.test(lastUser)) { await Promise.race([delay(20000), new Promise(r => res.once('close', r))]); if (res.destroyed) return; text = 'Saved **held.txt** after the held step.'; }
    else if (/activity/i.test(lastUser) && !toolDone) { text = slow.slice(0, 900); tail = { index: 0, id: randomUUID(), type: 'function', function: { name: 'create_text_file', arguments: JSON.stringify({ name: 'activity.txt', text: 'Activity notes' }) } }; }
    else if (/activity/i.test(lastUser)) text = slow.slice(0, 1500);
    else if (/tool/i.test(lastUser) && !toolDone) { text = 'Let me save that file first.'; tail = { index: 0, id: randomUUID(), type: 'function', function: { name: 'create_text_file', arguments: JSON.stringify({ name: 'notes.txt', text: 'Synthetic notes file' }) } }; }
    else if (/tool/i.test(lastUser)) text = 'Saved **notes.txt** to this conversation.';
    else if (/markdown|table|code/i.test(lastUser)) text = markdown;
    await delay(250);
    if (!body.stream) {
      return json(200, { choices: [{ index: 0, finish_reason: tail ? 'tool_calls' : 'stop', message: tail ? { role: 'assistant', content: text, tool_calls: [tail] } : { role: 'assistant', content: text } }], usage: { prompt_tokens: 10, completion_tokens: 10, total_tokens: 20 } });
    }
    await streamText(res, text, room ? 12 : 24, room ? 60 : (text === slow ? 60 : 35), tail);
  } catch (error) { if (!res.headersSent) res.writeHead(500); res.end(String(error)); }
});
await new Promise(r => provider.listen(0, '127.0.0.1', r));
const baseURL = `http://127.0.0.1:${provider.address().port}/v1`;
const portsFile = join(data, 'ports.json'); const ports = existsSync(portsFile) ? JSON.parse(readFileSync(portsFile, 'utf8')) : {};
const app = await startApp({ ...(ports.app ? { port: ports.app } : {}), database: join(data, 'minarbot.sqlite'), baseURL, apiKey: 'audit-synthetic', defaultModel: 'audit-model', stream: true, updates: { releasesDir: releases }, projectRoot: join(data, 'projects'), workspaces: [{ agent: coderId, directory: gitDir, write: true }], machines: { healthIntervalMs: 1000 }, ...(process.env.AUDIT_DESKTOP === '1' ? { desktop: { dataDir: data, enabled: true, leaseDir: join(data, 'leases') } } : {}) });
const peer = await startApp({ ...(ports.peer ? { port: ports.peer } : {}), database: join(data, 'peer.sqlite'), baseURL, apiKey: 'audit-synthetic', defaultModel: 'audit-model' });
const post = async (target, path, body) => {
  const r = await fetch(target.origin + path, { method: 'POST', headers: { Origin: target.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
  if (!r.ok) throw new Error(`${path} ${r.status} ${await r.text()}`); return r.json();
};
writeFileSync(portsFile, JSON.stringify({ app: Number(new URL(app.origin).port), peer: Number(new URL(peer.origin).port) }));
let paired = false;
if (fresh) {
await post(peer, '/api/environment', { id: randomUUID(), name: 'Studio workstation' });
await post(peer, '/api/agents', { runtime: 'api', id: randomUUID(), name: 'Remote Iris', model: 'audit-model', instructions: 'Synthetic peer agent.' });

const pairing = await post(peer, '/api/pairing-codes', { id: randomUUID() });
if (process.env.AUDIT_PAIR === '1') { await post(app, '/api/machines', { id: randomUUID(), origin: peer.origin, code: pairing.code }); paired = true; }
}
console.log(JSON.stringify({ origin: app.origin, peer: peer.origin, paired, coderId, pid: process.pid }));
const stop = () => { void Promise.all([app.close(), peer.close()]).finally(() => { provider.closeAllConnections(); provider.close(); process.exit(0); }); };
process.once('SIGTERM', stop); process.once('SIGINT', stop);

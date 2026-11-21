import { test } from 'node:test';
import assert from 'node:assert/strict';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { startApp } from './test-bridge.ts';

test('generated text artifacts persist across interruption without duplicate creation', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-artifacts-'));
  let hold = false;
  let releaseLate: (() => void) | undefined;
  let unrelatedFile = '', deniedLater = false;
  const provider = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk;
    const data = JSON.parse(raw), task = data.messages.findLast((m: { role: string }) => m.role === 'user').content;
    const result = data.messages.at(-1).role === 'tool' ? JSON.parse(data.messages.at(-1).content) : null;
    if (result && task === 'Earlier task' && unrelatedFile) {
      if (result.error) { deniedLater = result.error === 'Attachment not found in sent messages in this conversation.'; return res.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: 'Later unrelated file denied.' } }] })); }
      return res.end(JSON.stringify({ choices: [{ finish_reason: 'tool_calls', message: { tool_calls: [{ id: 'read-unrelated', type: 'function', function: { name: 'read_attachment', arguments: JSON.stringify({ id: unrelatedFile }) } }] } }] }));
    }
    if (result) {
      if (task === 'bad-path') assert.deepEqual(result, { error: 'Invalid text file name or content.', status: 400 });
      if (task === 'oversize') assert.deepEqual(result, { error: 'Text file limit is 32 KiB.', status: 413 });
      if (hold) { provider.emit('artifact-result-held', task); return; }
      return res.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: `Saved ${result.name}` } }] }));
    }
    if (task === 'Late file creation') await new Promise<void>(resolve => { releaseLate = resolve; provider.emit('artifact-call-held', task); });
    const args = { name: task === 'bad-path' ? '../escape.txt' : task === 'Write a file' ? 'result.txt' : `${task}.txt`, text: task === 'oversize' ? '界'.repeat(12000) : '<script>literal text</script>\nSYNTHETIC OUTPUT' };
    res.end(JSON.stringify({ choices: [{ finish_reason: 'tool_calls', message: { tool_calls: [{ id: 'artifact-call', type: 'function', function: { name: 'create_text_file', arguments: JSON.stringify(args) } }] } }] }));
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address(); assert(address && typeof address !== 'string');
  const options = { database: join(dir, 'state.sqlite'), baseURL: `http://127.0.0.1:${address.port}/v1` };
  let app = await startApp(options, { directory: dir, servers: [provider] });
  async function post(path: string, data: unknown, status?: number) {
    const response = await fetch(app.origin + path, { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(data) });
    if (status) assert.equal(response.status, status); else assert(response.ok, await response.clone().text()); return response.json();
  }
  async function get(path: string) { return (await fetch(app.origin + path)).json(); }
  async function holdTurn(path: string, id: string, text: string, event = 'artifact-result-held') {
    const [[task]] = await Promise.all([
      once(provider, event, { signal: AbortSignal.timeout(30_000) }),
      post(path, { id, text }),
    ]);
    assert.equal(task, text);
  }
  async function wait(path: string, id: string, status: string) {
    for (let i = 0; i < 150; i++) { const turn = (await get(path)).find((row: { id: string }) => row.id === id); if (turn?.status === status) return turn; await new Promise(resolve => setTimeout(resolve, 10)); }
    assert.fail(`Timed out waiting for ${status}`);
  }
  try {
    const agent = await post('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Writer', model: 'fixture', instructions: 'Help.' });
    const room = await post('/api/channels', { id: randomUUID(), name: 'Room', instructions: 'Help.', members: [agent.id], lead: agent.id });
    const side = await post(`/api/agents/${agent.id}/threads`, { id: randomUUID(), name: 'Side' });
    const paths = [`/api/agents/${agent.id}`, `/api/channels/${room.id}`, `/api/threads/${side.id}`];
    for (const path of paths) {
      const request = { id: randomUUID(), text: 'Write a file' };
      await post(`${path}/turns`, request); const turn = await wait(`${path}/turns`, request.id, 'done');
      assert.equal(turn.generatedFiles.length, 1); const file = turn.generatedFiles[0];
      const download = await fetch(`${app.origin}${path}/files/${file.id}`);
      assert.equal(download.headers.get('content-type'), 'application/octet-stream'); assert.match(download.headers.get('content-disposition')!, /attachment/);
      assert.equal(await download.text(), '<script>literal text</script>\nSYNTHETIC OUTPUT');
      await post(`${path}/turns`, request); assert.equal((await get(`${path}/files`)).length, 1);
      const foreign = paths.find(other => other !== path)!; assert.equal((await fetch(`${app.origin}${foreign}/files/${file.id}`)).status, 404);
    }
    const path = paths[0];
    for (const text of ['bad-path', 'oversize']) { const id = randomUUID(); await post(`${path}/turns`, { id, text }); await wait(`${path}/turns`, id, 'done'); }
    assert.equal((await get(`${path}/files`)).length, 1);
    hold = true;
    const id = randomUUID(); await holdTurn(`${path}/turns`, id, 'Retain on restart');
    await app.close(); app = await startApp(options);
    const interrupted = await wait(`${path}/turns`, id, 'interrupted'); assert.equal(interrupted.generatedFiles.length, 1);
    hold = false;
    await post(`${path}/turns/${id}/resume`, { id: randomUUID(), revision: interrupted.revision, acknowledgeUncertain: true });
    const resumed = await wait(`${path}/turns`, id, 'done'); assert.deepEqual(resumed.generatedFiles, interrupted.generatedFiles);
    assert.equal((await get(`${path}/files`)).length, 2);
    hold = true;
    const cancelledId = randomUUID(); await holdTurn(`${path}/turns`, cancelledId, 'Retain on cancellation');
    await post(`${path}/turns/${cancelledId}/cancel`, { id: randomUUID() });
    assert.equal((await wait(`${path}/turns`, cancelledId, 'cancelled')).generatedFiles.length, 1);
    const lateId = randomUUID(); await holdTurn(`${path}/turns`, lateId, 'Late file creation', 'artifact-call-held');
    assert(releaseLate); await post(`${path}/turns/${lateId}/cancel`, { id: randomUUID() });
    releaseLate(); hold = false;
    const sentinel = randomUUID(); await post(`${path}/turns`, { id: sentinel, text: 'Next task after cancellation' });
    await wait(`${path}/turns`, sentinel, 'done');
    assert.equal((await wait(`${path}/turns`, lateId, 'cancelled')).generatedFiles.length, 0);
    assert.equal((await get(`${path}/files`)).length, 4);
    hold = true;
    const earlierId = randomUUID(); await holdTurn(`${path}/turns`, earlierId, 'Earlier task');
    const earlier = (await get(`${path}/turns`)).find((row: { id: string }) => row.id === earlierId);
    await post(`${path}/turns/${earlierId}/pause`, { id: randomUUID(), revision: earlier.revision });
    hold = false;
    const laterId = randomUUID(); await post(`${path}/turns`, { id: laterId, text: 'Unrelated later task' });
    unrelatedFile = (await wait(`${path}/turns`, laterId, 'done')).generatedFiles[0].id;
    const paused = (await get(`${path}/turns`)).find((row: { id: string }) => row.id === earlierId);
    await post(`${path}/turns/${earlierId}/resume`, { id: randomUUID(), revision: paused.revision, acknowledgeUncertain: true });
    await wait(`${path}/turns`, earlierId, 'done'); assert(deniedLater);


  } finally { releaseLate?.(); await app.close(); provider.closeAllConnections(); await new Promise<void>(resolve => provider.close(() => resolve())); rmSync(dir, { recursive: true, force: true }); }
});

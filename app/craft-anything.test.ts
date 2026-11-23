import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import test from 'node:test';
import { startApp } from './test-bridge.ts';

const linux = process.platform === 'linux';
test('Craft Anything is built in and maps its approvals to allow-once cards', { skip: !linux }, async () => verifyCraft(false));
test('Craft Anything installs its signed release and captures a public page in Computer', {
  skip: !linux || process.env.MINARBOT_CRAFT_LIVE !== '1',
}, async () => verifyCraft(true));

async function verifyCraft(downloadRelease: boolean) {
  const directory = await mkdtemp(join(tmpdir(), 'minarbot-craft-'));
  let finish: () => void = () => {};
  const held = new Promise<void>(resolve => { finish = resolve; });
  const provider = createServer(async (req, res) => {
    for await (const _ of req) { /* consume request */ }
    await held;
    res.setHeader('Content-Type', 'application/json');
    res.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: 'Done.' } }] }));
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address();
  assert(address && typeof address !== 'string');
  const app = await startApp({
    database: join(directory, 'state.sqlite'), baseURL: `http://127.0.0.1:${address.port}/v1`, defaultModel: 'fixture',
    desktop: { enabled: true, dataDir: join(directory, 'desktop'), leaseDir: join(directory, 'leases') },
  }, { directory, servers: [provider] });
  const agent = randomUUID(), turn = randomUUID();
  const path = `/api/agents/${agent}/turns`, settings = '/api/settings/craft-anything';
  const headers = { Origin: app.origin, 'Content-Type': 'application/json' };
  async function request(url: string, body?: unknown, expected = 200) {
    const response = await fetch(app.origin + url, { method: body === undefined ? 'GET' : 'POST', headers, body: body === undefined ? undefined : JSON.stringify(body) });
    assert.equal(response.status, expected, await response.clone().text());
    return response.json();
  }
  async function waitForTurn(check: (row: { status: string; harnessApprovals: { state: string }[] }) => boolean) {
    const until = Date.now() + 20_000;
    do {
      const row = (await request(path)).find((row: { id: string }) => row.id === turn);
      if (row && check(row)) return row;
      assert.notEqual(row?.status, 'failed', row?.error);
      await delay(50);
    } while (Date.now() < until);
    assert.fail('Craft Anything turn did not reach the expected state');
  }
  try {
    const initial = await request(settings);
    assert.equal(initial.installed, false);
    if (!downloadRelease) {
      const version = join(directory, 'craft-anything', initial.version);
      await mkdir(version, { recursive: true });
      await writeFile(join(version, 'craft-anything-cli'), 'fixture');
    }
    await request('/api/agents', { runtime: 'api', id: agent, name: 'Craft fixture', model: 'fixture', instructions: 'Study websites.' }, 201);
    await request(path, { id: turn, text: 'Check Craft Anything.' }, 202);
    await waitForTurn(row => row.status === 'running');
    const session = app.mcpServer.mintAgentSession({ agent, turn, scopes: ['orchestration'], ttlMs: 180_000 });
    async function rpc(method: string, params: object = {}, expected = 200) {
      const response = await fetch(app.origin + '/mcp', {
        method: 'POST', headers: { ...headers, Authorization: `Bearer ${session.token}` },
        body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
      });
      assert.equal(response.status, expected, await response.clone().text());
      const payload = await response.json();
      return expected === 200 ? payload.result : payload;
    }
    const exposed = async () => (await rpc('tools/list')).tools.map((item: { name: string }) => item.name);
    const list = (args: object) => rpc('tools/call', { name: 'list_craft_tools', arguments: args });
    const call = (name: string, args: object) => rpc('tools/call', { name: 'call_craft_tool', arguments: { name, arguments: args } });
    async function decide(decision: string) {
      const row = await waitForTurn(row => row.harnessApprovals.some(card => card.state === 'approval'));
      const card = row.harnessApprovals.find((card: { state: string }) => card.state === 'approval');
      assert.equal(card.kind, 'craft');
      assert.equal(card.prefix, null);
      const url = `${path}/${turn}/harness-approvals/${card.id}`;
      await request(url, { id: randomUUID(), revision: card.revision, decision: 'always' }, 400);
      await request(url, { id: randomUUID(), revision: card.revision, decision });
      return card.input;
    }
    const names = await exposed();
    assert(names.includes('list_craft_tools'));
    assert(names.includes('call_craft_tool'));
    for (const enabled of [true, false]) {
      assert((await request(settings + '/grant', { agent, enabled })).agents.includes(agent));
    }
    assert.deepEqual(await exposed(), names);
    assert.equal((await rpc('tools/call', { name: 'list_craft_tools', arguments: { deep: true } }, 403)).error,
      'MCP authorization failed.');
    const input = { file_path: 'draft.txt' };
    for (const decision of ['deny', 'allow']) {
      const pending = call('core_action_approve', { tool_name: 'Write', input });
      assert.deepEqual(await decide(decision), { tool: 'Write', arguments: input });
      const result = (await pending).structuredContent;
      assert.equal(result.behavior, decision);
      if (decision === 'allow') assert.deepEqual(result.updatedInput, input);
    }
    if (downloadRelease) {
      assert.equal((await list({ name: 'core_target_open' })).structuredContent.tool, null);
      assert.equal((await list({ name: 'web_site_open' })).structuredContent.tool.name, 'web_site_open');
      assert.equal((await request(settings)).installed, true);
      assert((await request('/api/skills')).skills.some((row: { name: string }) => row.name === 'Craft Anything'));
      const pending = call('web_site_open', { url: 'https://example.com/', settle_ms: 300 });
      await decide('allow');
      assert.match(JSON.stringify(await pending), /Example Domain/);
      const captured = await call('web_site_capture', { output_directory: '/work/capture', widths: [390], include_html: true });
      const evidence = captured.structuredContent.structuredContent;
      assert.equal(evidence.environment.clean_room, true);
      assert.equal(evidence.params.include_html, false);
      const artifact = await call('export_craft_artifact', { path: '/work/capture/screenshot-390.png' });
      assert.equal(artifact.structuredContent.name, 'screenshot-390.png');
      console.log(`Live Computer clean-room capture ${evidence.id}`);
    }
    assert.equal((await request(settings + '/remove', {})).installed, false);
    assert.deepEqual(await exposed(), names);
  } finally {
    finish();
    await app.close();
    provider.closeAllConnections();
    await new Promise<void>(resolve => provider.close(() => resolve()));
    await rm(directory, { recursive: true });
  }
}

import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync, mkdtempSync, writeFileSync, chmodSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { randomUUID } from 'node:crypto';
import { startApp } from './test-bridge.ts';
// Retained for B8 (docs/test-ledger.md); the other cases moved to
// crates/minarbot-e2e/tests/e2e/sb_harness_default.rs.
test('provider isolation, account home rotation and MCP shell approval boundaries', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'minarbot-provider-home-')), executable = join(directory, 'codex');
  writeFileSync(executable, `#!${process.execPath}\n` + readFileSync(new URL('./fixtures/fake-harness.mjs', import.meta.url), 'utf8').split('\n').slice(1).join('\n')); chmodSync(executable, 0o700);
  const accounts = ['first', 'second'].map(id => ({ id, name: id, provider: 'openai-compatible', baseURL: 'http://127.0.0.1:1/v1', apiKey: `synthetic-${id}-credential` }));
  const app = await startApp({ database: join(directory, 'state.sqlite'), accounts, harness: { executables: { codex: executable } } });
  const api = async (path: string, body?: unknown) => { const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined }); const data = await response.json(); assert(response.ok, JSON.stringify(data)); return data; };
  let homes: string[] = [];
  try {
    const agent = await api('/api/agents', { id: randomUUID(), name: 'Coder', model: 'router/model', providerAccount: 'first', instructions: 'Help.' });
    const path = `/api/agents/${agent.id}`, workspace = join(directory, 'agent-workspaces', agent.id);
    for (const [index, account] of accounts.entries()) {
      if (index) await api(path + '/edit', { id: randomUUID(), revision: 1, name: agent.name, model: agent.model, instructions: agent.instructions, providerAccount: account.id });
      const id = randomUUID(); await api(path + '/turns', { id, text: 'FINAL_PROVIDER_SECRET' });
      let row;
      for (let n = 0; n < 300; n++) { row = (await api(path + '/turns')).find((item: { id: string }) => item.id === id); if (['done', 'failed'].includes(row.status)) break; await new Promise(resolve => setTimeout(resolve, 10)); }
      assert.equal(row.status, 'done', row.error); assert(!JSON.stringify(row).includes(account.apiKey)); assert.match(row.reply, /REDACTED/);
      const log = readFileSync(join(workspace, 'fake-log.jsonl'), 'utf8').trim().split('\n').map(line => JSON.parse(line));
      homes = log.filter(item => item.method === 'thread/start').map(item => item.home);
      const config = readFileSync(join(homes.at(-1)!, '.codex/config.toml'), 'utf8');
      assert.match(config, /router\/model/); assert(!config.includes(account.apiKey));
    }
    assert.equal(homes.length, 2); assert.notEqual(homes[0], homes[1]); assert(!existsSync(homes[0]));
    const turn = randomUUID(); await api(path + '/turns', { id: turn, text: 'HOLD' });
    for (let n = 0; n < 300; n++) { if ((await api(path + '/turns')).find((item: { id: string }) => item.id === turn)?.partial?.text) break; await new Promise(resolve => setTimeout(resolve, 10)); }
    const session = app.mcpServer.mintAgentSession({ agent: agent.id, turn, ttlMs: 60000, scopes: ['orchestration'] });
    const catalogResponse = await fetch(app.origin + '/mcp', { method: 'POST', headers: { Authorization: `Bearer ${session.token}`, 'Content-Type': 'application/json', Accept: 'application/json, text/event-stream' }, body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/list' }) });
    const catalog = (await catalogResponse.json()).result.tools.map((tool: { name: string }) => tool.name);
    for (const name of ['run_shell', 'verify_result', 'create_text_file', 'read_workspace_file', 'open_file_in_panel']) assert(catalog.includes(name), name);
    for (const name of ['recall', 'reflect', 'remember', 'search_history', 'search_memory', 'remember_memory', 'select_memory']) assert(catalog.includes(name), name);
    for (const name of ['create_workspace_file', 'git_status', 'set_routine_flow']) assert(!catalog.includes(name), name);
    const call = async (name: string, args: Record<string, unknown>) => {
      const response = await fetch(app.origin + '/mcp', { method: 'POST', headers: { Authorization: `Bearer ${session.token}`, 'Content-Type': 'application/json', Accept: 'application/json, text/event-stream' }, body: JSON.stringify({ jsonrpc: '2.0', id: randomUUID(), method: 'tools/call', params: { name, arguments: args } }) });
      return (await response.json()).result;
    };
    const shell = (command: string) => call('run_shell', { command });
    const normal = await shell('echo hello > result; cat result'); assert.equal(normal.structuredContent.exitCode, 0);
    assert.deepEqual(normal.structuredContent.verification, { recorded: true, evidenceInWorkLog: true });
    const custom = await call('run_shell', { command: 'python3 -c "assert 2 + 2 == 4"', check: true });
    assert.equal(custom.structuredContent.exitCode, 0);
    assert.deepEqual(custom.structuredContent.verification, { recorded: true, evidenceInWorkLog: true });
    assert.equal(readFileSync(join(workspace, 'result'), 'utf8'), 'hello\n');
    assert.equal((await api(path + '/turns')).find((item: { id: string }) => item.id === turn).harnessApprovals.length, 0);
    const sensitive = shell('rm result');
    let card;
    for (let n = 0; n < 300; n++) { card = (await api(path + '/turns')).find((item: { id: string }) => item.id === turn)?.harnessApprovals?.find((item: { state: string }) => item.state === 'approval'); if (card) break; await new Promise(resolve => setTimeout(resolve, 10)); }
    assert(card); await api(`${path}/turns/${turn}/harness-approvals/${card.id}`, { id: randomUUID(), revision: card.revision, decision: 'deny' });
    assert((await sensitive).isError); assert(existsSync(join(workspace, 'result')));
    await call('ask_user', { question: 'Which report?', choices: ['Weekly', 'Daily'] });
    const paused = (await api(path + '/turns')).find((item: { id: string }) => item.id === turn); assert.equal(paused.paused, 1); assert.equal(paused.questions[0].prompt, 'Which report?');
    await api(`${path}/turns/${turn}/questions/${paused.questions[0].id}/answer`, { id: randomUUID(), text: 'Weekly' });
    await api(`${path}/turns/${turn}/cancel`, { id: randomUUID() });

  } finally { await app.close(); for (const home of homes) assert(!existsSync(home)); rmSync(directory, { recursive: true, force: true }); }
});

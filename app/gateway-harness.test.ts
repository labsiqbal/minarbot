import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { fileURLToPath } from 'node:url';

const fixture = fileURLToPath(new URL('./fixtures/fake-gateway-harness.mjs', import.meta.url));

async function runGateway(mode: 'stream' | 'tool' | 'error') {
  let calls = 0;
  const key = 'gateway-secret-fixture-key';
  const provider = createServer(async (request, response) => {
    assert.equal(request.headers.authorization, `Bearer ${key}`);
    let raw = '';
    for await (const chunk of request) raw += chunk;
    const body = JSON.parse(raw);
    calls++;
    if (mode === 'error') {
      response.writeHead(503).end();
      return;
    }
    if (mode === 'tool' && calls === 1) {
      response.end(JSON.stringify({ choices: [{ finish_reason: 'tool_calls', message: { tool_calls: [{ id: 'echo-1', type: 'function', function: { name: 'echo', arguments: '{"value":"round trip"}' } }] } }] }));
      return;
    }
    if (mode === 'tool') assert.equal(body.messages.at(-1).role, 'tool');
    if (mode === 'stream') {
      response.writeHead(200, { 'Content-Type': 'text/event-stream' });
      response.write(`data: ${JSON.stringify({ choices: [{ delta: { content: 'streamed ' } }] })}\n\n`);
      response.write(`data: ${JSON.stringify({ choices: [{ delta: { content: 'answer' }, finish_reason: 'stop' }] })}\n\n`);
      response.end('data: [DONE]\n\n');
      return;
    }
    response.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: mode === 'tool' ? 'tool answer' : 'unused' } }] }));
  });
  const mcp = createServer(async (request, response) => {
    assert.equal(request.headers.authorization, 'Bearer mcp-session-secret');
    let raw = '';
    for await (const chunk of request) raw += chunk;
    const body = JSON.parse(raw);
    const result = body.method === 'tools/list'
      ? { tools: [{ name: 'echo', description: 'Echo fixture', inputSchema: { type: 'object' } }] }
      : { content: [{ type: 'text', text: 'echo result' }], structuredContent: { value: 'echo result' } };
    response.setHeader('Content-Type', 'application/json');
    response.end(JSON.stringify({ jsonrpc: '2.0', id: body.id, result }));
  });
  await Promise.all([
    new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve)),
    new Promise<void>(resolve => mcp.listen(0, '127.0.0.1', resolve)),
  ]);
  const providerAddress = provider.address();
  const mcpAddress = mcp.address();
  assert(providerAddress && typeof providerAddress !== 'string');
  assert(mcpAddress && typeof mcpAddress !== 'string');
  const child = spawn(process.execPath, [fixture], {
    cwd: process.cwd(),
    env: {
      ...process.env,
      MINARBOT_GATEWAY_BASE_URL: `http://127.0.0.1:${providerAddress.port}/v1`,
      MINARBOT_PROVIDER_KEY: key,
      MINARBOT_GATEWAY_MODEL: 'fixture',
      MINARBOT_GATEWAY_STREAM: String(mode === 'stream'),
      MINARBOT_MCP_URL: `http://127.0.0.1:${mcpAddress.port}/mcp`,
      MINARBOT_MCP_TOKEN: 'mcp-session-secret',
      MINARBOT_TOOL_BUDGET: '10',
    },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  child.stdout.setEncoding('utf8');
  child.stderr.setEncoding('utf8');
  let output = '', errors = '';
  child.stdout.on('data', value => { output += value; });
  child.stderr.on('data', value => { errors += value; });
  const lines: string[] = [];
  child.stdout.on('data', value => lines.push(...value.trim().split('\n').filter(Boolean)));
  const next = async () => {
    while (!lines.length) await once(child.stdout, 'data');
    return JSON.parse(lines.shift()!);
  };
  const send = (value: unknown) => child.stdin.write(JSON.stringify(value) + '\n');
  send({ id: 1, method: 'initialize', params: {} });
  assert.equal((await next()).id, 1);
  send({ id: 2, method: 'thread/start', params: { threadId: 'fixture-thread', config: { mcp_servers: { minarbot: { url: `http://127.0.0.1:${mcpAddress.port}/mcp`, bearer_token_env_var: 'MINARBOT_MCP_TOKEN' } } } } });
  assert.equal((await next()).id, 2);
  send({ id: 3, method: 'turn/start', params: { threadId: 'fixture-thread', input: [{ type: 'text', text: 'Hello' }], additionalContext: {} } });
  assert.equal((await next()).id, 3);
  const events = [];
  for (;;) {
    const value = await next();
    events.push(value);
    if (value.method === 'turn/completed' || value.method === 'error') break;
  }
  child.kill('SIGTERM');
  await once(child, 'close');
  provider.closeAllConnections(); mcp.closeAllConnections();
  await Promise.all([
    new Promise<void>(resolve => provider.close(() => resolve())),
    new Promise<void>(resolve => mcp.close(() => resolve())),
  ]);
  return { calls, events, output, errors, key };
}

test('gateway streams provider text as Codex events', async () => {
  const result = await runGateway('stream');
  assert.equal(result.calls, 1);
  assert(result.events.some(value => value.method === 'item/agentMessage/delta' && value.params.delta === 'streamed '));
  assert(result.events.some(value => value.method === 'turn/completed'));
  assert(!result.output.includes(result.key));
  assert(!result.errors.includes(result.key));
});

test('gateway completes MCP tool round trip before final text', async () => {
  const result = await runGateway('tool');
  assert.equal(result.calls, 2);
  assert(result.events.some(value => value.method === 'item/completed' && value.params.item.type === 'mcpToolCall'));
  assert(result.events.some(value => value.method === 'item/agentMessage/delta' && value.params.delta === 'tool answer'));
});

test('gateway provider errors become Codex turn errors without leaking key', async () => {
  const result = await runGateway('error');
  assert(result.events.some(value => value.method === 'error'));
  assert(!result.output.includes(result.key));
  assert(!result.errors.includes(result.key));
});

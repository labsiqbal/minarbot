#!/usr/bin/env node
// Fake native peer. No model calls, no host configuration reads.
import assert from 'node:assert/strict';
import { appendFileSync, existsSync } from 'node:fs';
import { createInterface } from 'node:readline';
import { spawn, spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
if (process.argv.includes('--version')) { console.log('fake-harness 1.0.0'); process.exit(0); }
// Sign-in status: signed in when the engine folder holds the CLI's credential file.
if (process.argv[2] === 'login' && process.argv[3] === 'status') process.exit(existsSync(`${process.env.CODEX_HOME}/auth.json`) ? 0 : 1);
if (process.argv[2] === 'auth' && process.argv[3] === 'status') process.exit(existsSync(`${process.env.CLAUDE_CONFIG_DIR ?? `${process.env.HOME}/.claude`}/.credentials.json`) ? 0 : 1);
for (const key of ['OPENAI_API_KEY','ANTHROPIC_API_KEY','COMPOSIO_API_KEY','MINARBOT_SCRIPT_TOKEN','NODE_OPTIONS','MINARBOT_HARNESS_CANARY']) assert.equal(process.env[key], key === 'ANTHROPIC_API_KEY' && process.env[key] === 'minarbot-no-key' ? 'minarbot-no-key' : undefined, `${key} leaked`);
const codex = process.argv.includes('app-server');
const arg = flag => { const at = process.argv.indexOf(flag); return at >= 0 ? process.argv[at + 1] : process.argv.find(value => value.startsWith(flag + '='))?.slice(flag.length + 1); };
const log = value => appendFileSync('fake-log.jsonl', JSON.stringify(value) + '\n');
const send = value => process.stdout.write(JSON.stringify(value) + '\n');
const hasResume = process.argv.some(value => value === '--resume' || value.startsWith('--resume='));
let mcpURL = process.env.MINARBOT_MCP_URL;
let session = (hasResume ? (arg('--resume') ?? process.argv.find(value => value.startsWith('--resume='))?.slice(9)) : undefined) ?? arg('--session-id') ?? randomUUID(), prompt = '', callback = '', stopCallback = '', approvalId = '';
const notify = (method, params) => send({ method, params: { threadId: session, ...params } });
function preToolUse(id, name, input) {
  send({ type: 'control_request', request_id: id, request: { subtype: 'hook_callback', callback_id: callback,
    input: { session_id: session, cwd: process.cwd(), hook_event_name: 'PreToolUse', tool_name: name, tool_input: input, tool_use_id: id } } });
}
const workspaceShell = {
  command: 'ls -al && for file in *.test.mjs; do head -5 "$file"; done && node --test fixture.test.mjs && printf checked > result.txt',
  check: true,
};
function runWorkspaceShell() {
  const { command, args } = JSON.parse(arg('--mcp-config')).mcpServers.minarbot;
  const result = spawnSync(command, args, {
    input: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/call', params: {
      name: 'run_shell', arguments: workspaceShell,
    } }) + '\n',
    encoding: 'utf8', timeout: 5000,
  });
  assert.equal(result.status, 0);
  const receipt = JSON.parse(result.stdout).result;
  assert.equal(receipt.isError, undefined);
  assert.equal(receipt.structuredContent.exitCode, 0);
}
function delta(text) { if (codex) notify('item/agentMessage/delta', { delta: text }); else send({ type: 'stream_event', session_id: session, uuid: randomUUID(), parent_tool_use_id: null, event: { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text } } }); }
function finish(allowed = true) {
  const check = prompt.includes('NATIVE_CHECK');
  if (codex) {
    if (check) notify('item/completed', { item: { id: 'edit', type: 'fileChange', status: 'completed' } });
    notify('item/completed', { item: { id: 'shell', type: 'commandExecution', command: check ? 'node --test fixture.test.mjs' : 'echo hello', status: allowed ? 'completed' : 'declined', ...(check ? { aggregatedOutput: 'Native check passed.', exitCode: 0 } : {}) } });
    delta(allowed ? 'finished' : 'denied');
    if (prompt.includes('FINAL_SECRET')) { delta(' test-mcp-session-'); delta('token'); }
    if (prompt.includes('FINAL_PROVIDER_SECRET') && process.env.MINARBOT_PROVIDER_KEY) { const key = process.env.MINARBOT_PROVIDER_KEY; delta(key.slice(0, 8)); delta(key.slice(8)); }
    notify('thread/tokenUsage/updated', { tokenUsage: { last: { inputTokens: 11, outputTokens: 7 } } });
    notify('turn/completed', { turn: { id: 'fake-turn', status: 'completed' } });
  } else {
    if (check) {
      send({ type: 'assistant', message: { id: 'edit', content: [{ type: 'tool_use', id: 'edit', name: 'Write', input: { file_path: 'result.txt', content: 'checked' } }] } });
      send({ type: 'user', message: { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'edit', content: 'File saved.' }] } });
    }
    if (!prompt.includes('WORKSPACE_SHELL_FIXTURE')) {
      send({ type: 'assistant', uuid: randomUUID(), session_id: session, parent_tool_use_id: null, message: { id: 'm1', role: 'assistant', type: 'message', content: [{ type: 'tool_use', id: 'shell', name: 'Bash', input: { command: check ? 'node --test fixture.test.mjs' : 'echo hello' } }], model: 'fake', stop_reason: 'tool_use', stop_sequence: null, usage: { input_tokens: 11, output_tokens: 7 } } });
      send({ type: 'user', uuid: randomUUID(), session_id: session, parent_tool_use_id: null, message: { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'shell', content: 'fake result', is_error: !allowed }] } });
    }
    delta(allowed ? 'finished' : 'denied');
    if (prompt.includes('FINAL_SECRET')) { delta(' test-mcp-session-'); delta('token'); }
    send({ type: 'result', subtype: 'success', is_error: false, result: 'fake result', session_id: session, uuid: randomUUID(), duration_ms: 5, duration_api_ms: 5, num_turns: 1, total_cost_usd: 0, usage: { input_tokens: 11, output_tokens: 7, cache_read_input_tokens: 0, cache_creation_input_tokens: 0 }, modelUsage: {}, permission_denials: [] });
  }
}
async function run(text) {
  if (text.includes('STREAM_GATE') || text.includes('STREAM_CRASH')) { void streamGate(text); return; }
  prompt = text; log({ type: 'turn', session, prompt, home: process.env.HOME, claudeConfigDir: process.env.CLAUDE_CONFIG_DIR ?? null });
  if (!codex && text.includes('WORKSPACE_SHELL_FIXTURE')) {
    preToolUse('workspace-ls', 'Bash', { command: 'ls -al', description: 'Inspect workspace' });
    return;
  }
  delta('Harness ');
  if (text.includes('FIRST_PARTIAL_GATE')) {
    while (!existsSync('stream-release')) await new Promise(resolve => setTimeout(resolve, 5));
  }
  if (!codex && text.includes('Create a deliverable.')) { send({ type: 'control_request', request_id: 'sv-stop', request: { subtype: 'hook_callback', callback_id: stopCallback, input: { session_id: session, transcript_path: '', cwd: process.cwd(), hook_event_name: 'Stop', stop_hook_active: false, last_assistant_message: 'Done.' } } }); return; }
  if (text.includes('NATIVE_SETUP_FIXTURE') && text.includes('Initial needs:')) { void (async()=>{ const rpc=async(name,args)=>{const r=await fetch(mcpURL,{method:'POST',headers:{Authorization:`Bearer ${process.env.MINARBOT_MCP_TOKEN}`,'Content-Type':'application/json',Accept:'application/json, text/event-stream'},body:JSON.stringify({jsonrpc:'2.0',id:randomUUID(),method:'tools/call',params:{name,arguments:args}})});return {status:r.status,body:await r.json()};}; assert.equal((await rpc('create_text_file',{name:'forbidden',text:'x'})).status,403); const result=await rpc('edit_profile',{revision:1,name:'Native assistant',instructions:'Native setup saved.'});assert(result.body.result,JSON.stringify(result));assert.equal(result.body.result.structuredContent.revision,2,JSON.stringify(result));finish(); })().catch(error=>{log({setupError:String(error)});process.exit(1);});return; }
  if (text.includes('CRASH')) { setTimeout(() => process.exit(19), 20); return; }
  if (text.includes('OVERSIZE')) { process.stdout.write('x'.repeat(1024 * 1024 + 1)); return; }
  if (text.includes('HOLD') && !text.includes('RESTART_FINISH')) {
    const child = spawn(process.execPath, ['-e', 'setInterval(()=>{},1000)'], { stdio: 'ignore' }); log({ type: 'descendant', pid: child.pid }); return;
  }
  if (codex) {
    notify('item/started', { item: { id: 'shell', type: 'commandExecution', command: 'echo hello' } });
    if (text.includes('NETWORK') || text.includes('OUTSIDE_FILE') || text.includes('READ_ONLY_WRITE')) {
      approvalId = 'approval-native'; send({id:approvalId,method:text.includes('NETWORK') ? 'item/permissions/requestApproval' : 'item/fileChange/requestApproval',params:{threadId:session,turnId:'fake-turn',itemId:'shell',reason:'Allow requested access?',...(text.includes('NETWORK') ? {permissions:{network:{enabled:true}}} : {grantRoot:text.includes('OUTSIDE_FILE') ? '/outside' : process.cwd()})}}); return;
    }
    if (text.includes('APPROVE')) { approvalId = 'approval-native'; send({ id: approvalId, method: 'item/commandExecution/requestApproval', params: { threadId: session, turnId: 'fake-turn', itemId: 'shell', command: text.includes('APPROVE_SECRET') ? 'echo test-mcp-session-token' : 'echo hello', reason: 'Run command outside sandbox?', cwd: process.cwd() } }); return; }
  } else if (text.includes('OUTSIDE_FILE') || text.includes('NETWORK') || text.includes('WORKSPACE_WRITE') || text.includes('READ_ONLY_WRITE') || text.includes('APPROVE')) {
    approvalId = 'approval-native'; send({ type: 'control_request', request_id: approvalId, request: { subtype: 'hook_callback', callback_id: callback, input: { session_id: session, transcript_path: '', cwd: process.cwd(), hook_event_name: 'PreToolUse', tool_name: text.includes('NETWORK') ? 'WebFetch' : /OUTSIDE_FILE|WORKSPACE_WRITE|READ_ONLY_WRITE/.test(text) ? 'Write' : 'Bash', tool_input: text.includes('NETWORK') ? {url:'https://example.invalid/'} : /OUTSIDE_FILE|WORKSPACE_WRITE|READ_ONLY_WRITE/.test(text) ? {file_path:text.includes('OUTSIDE_FILE') ? '/outside/hello.txt' : process.cwd()+'/hello.txt',content:'hello'} : { dangerouslyDisableSandbox: true, command: text.includes('APPROVE_SECRET') ? 'echo test-mcp-session-token' : 'echo hello' }, tool_use_id: 'shell' } } }); return;
  }
  setTimeout(() => finish(), 100);
}
async function streamGate(text) {
  log({ type: 'turn', session, prompt: text });
  const start = id => {
    if (!codex) send({ type: 'stream_event', session_id: session, uuid: randomUUID(), parent_tool_use_id: null, event: { type: 'message_start', message: { id, type: 'message', role: 'assistant', content: [], model: 'fake', stop_reason: null, stop_sequence: null, usage: { input_tokens: 1, output_tokens: 0 } } } });
  };
  const emit = (id, value) => {
    if (codex) notify('item/agentMessage/delta', { itemId: id, delta: value }); else delta(value);
  };
  start('before');
  emit('before', 'Before tool. ');
  for (let i = 0; i < 100; i++) emit('before', '🧪');
  emit('before', 'test-mcp-session-'); emit('before', 'token');
  if (codex) notify('item/started', { item: { id: 'gate-tool', type: 'commandExecution', command: 'echo gate' } });
  else send({ type: 'assistant', uuid: randomUUID(), session_id: session, parent_tool_use_id: null, message: { id: 'before', role: 'assistant', type: 'message', content: [{ type: 'tool_use', id: 'gate-tool', name: 'Read', input: { file_path: 'fixture.txt' } }], model: 'fake', stop_reason: 'tool_use', stop_sequence: null, usage: { input_tokens: 1, output_tokens: 1 } } });
  start('answer'); emit('answer', 'Draft ');
  for (let i = 0; i < 100; i++) emit('answer', 'x');
  log({ type: 'stream-gate' });
  while (!existsSync('stream-release')) await new Promise(resolve => setTimeout(resolve, 5));
  if (text.includes('STREAM_CRASH')) { emit('answer', ' buffered tail'); process.stdout.write('', () => process.exit(19)); return; }
  if (codex) {
    notify('item/completed', { item: { id: 'gate-tool', type: 'commandExecution', command: 'echo gate', status: 'completed' } });
    notify('item/completed', { item: { id: 'answer', type: 'agentMessage', text: 'Final answer.' } });
    notify('turn/completed', { turn: { id: 'fake-turn', status: 'completed' } });
  } else {
    send({ type: 'user', uuid: randomUUID(), session_id: session, parent_tool_use_id: null, message: { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'gate-tool', content: 'Done' }] } });
    send({ type: 'assistant', uuid: randomUUID(), session_id: session, parent_tool_use_id: null, message: { id: 'answer', role: 'assistant', type: 'message', content: [{ type: 'text', text: 'Final answer.' }], model: 'fake', stop_reason: 'end_turn', stop_sequence: null, usage: { input_tokens: 1, output_tokens: 1 } } });
    send({ type: 'result', subtype: 'success', is_error: false, result: 'Final answer.', session_id: session, uuid: randomUUID(), duration_ms: 5, duration_api_ms: 5, num_turns: 1, total_cost_usd: 0, usage: { input_tokens: 1, output_tokens: 1, cache_read_input_tokens: 0, cache_creation_input_tokens: 0 }, modelUsage: {}, permission_denials: [] });
  }
}
for await (const line of createInterface({ input: process.stdin })) {
  const value = JSON.parse(line);
  if (codex) {
    log({ method: value.method ?? 'response', home: process.env.HOME, codexHome: process.env.CODEX_HOME });
    if (value.method === 'initialize') send({ id: value.id, result: { userAgent: 'fake' } });
    if (['thread/start','thread/resume'].includes(value.method)) {
      mcpURL = value.params.config.mcp_servers.minarbot.url; assert.equal(value.params.cwd, process.cwd()); assert.match(value.params.config.mcp_servers.minarbot.url, /^http:\/\/127\.0\.0\.1:\d+\/mcp$/);
      assert.equal(value.params.config.mcp_servers.minarbot.bearer_token_env_var, 'MINARBOT_MCP_TOKEN'); assert(process.env.MINARBOT_MCP_TOKEN);
      assert.equal(value.params.approvalPolicy, 'on-request'); assert.equal(value.params.config.approvals_reviewer, 'user');
      assert.equal(value.params.config['shell_environment_policy.inherit'], 'none');
      assert(!JSON.stringify(value.params.config['shell_environment_policy.set']).includes(process.env.MINARBOT_MCP_TOKEN));
      assert(!process.env.MINARBOT_PROVIDER_KEY || !JSON.stringify(value.params.config['shell_environment_policy.set']).includes(process.env.MINARBOT_PROVIDER_KEY));
      session = value.params.threadId ?? session; send({ id: value.id, result: { thread: { id: session } } });
    }
    if (value.method === 'turn/start') { log({ context: value.params.additionalContext }); assert.equal(value.params.threadId, session); assert.equal(value.params.sandboxPolicy.networkAccess ?? false, false); if(value.params.sandboxPolicy.type==='workspaceWrite'){assert.deepEqual(value.params.sandboxPolicy.writableRoots,[process.cwd()]);assert.equal(value.params.sandboxPolicy.excludeTmpdirEnvVar,true);assert.equal(value.params.sandboxPolicy.excludeSlashTmp,true);} send({ id: value.id, result: { turn: { id: 'fake-turn', status: 'inProgress' } } }); notify('turn/started', { turn: { id: 'fake-turn' } }); run(value.params.input[0].text); }
    if (value.id === approvalId && value.result) { log({ decision: value.result.decision }); finish(value.result.decision === 'accept' || !!value.result.permissions?.network); }
    if (value.method === 'turn/interrupt') { log({ interrupted: true }); send({ id: value.id, result: {} }); }
  } else {
    if (value.type === 'control_request') {
      const request = value.request;
      if (request.subtype === 'initialize') {
        callback = request.hooks?.PreToolUse?.[0]?.hookCallbackIds?.[0] ?? '';
        stopCallback = request.hooks?.Stop?.[0]?.hookCallbackIds?.[0] ?? '';
        log({ type: 'init', resume: hasResume, callback: !!callback, context: arg('--append-system-prompt') ?? request.appendSystemPrompt });
        const config = JSON.parse(arg('--mcp-config')); assert.equal(config.mcpServers.minarbot.type, 'stdio'); assert(process.env.MINARBOT_MCP_TOKEN); assert(!JSON.stringify(config).includes(process.env.MINARBOT_MCP_TOKEN));
        send({ type: 'control_response', response: { subtype: 'success', request_id: value.request_id, response: { commands: [], models: [], account: {}, hook_on_failure_supported: true, hooks_applied: true } } });
      } else if (request.subtype === 'interrupt') { log({ interrupted: true }); send({ type: 'control_response', response: { subtype: 'success', request_id: value.request_id, response: {} } }); }
      else send({ type: 'control_response', response: { subtype: 'success', request_id: value.request_id, response: {} } });
    }
    if (value.type === 'user') {
      send({ type: 'system', subtype: 'init', session_id: session, uuid: randomUUID(), cwd: process.cwd(), tools: [], mcp_servers: [], model: 'fake', permissionMode: 'default', apiKeySource: 'none', slash_commands: [] });
      run(typeof value.message.content === 'string' ? value.message.content : value.message.content.map(block => block.text ?? '').join('\n'));
    }
    if (value.type === 'control_response' && value.response.request_id === 'sv-stop') { log({ type: 'stop-hook', decision: value.response.response?.decision }); run(value.response.response?.reason ?? 'No reminder.'); }
    if (value.type === 'control_response' && value.response.request_id === 'workspace-ls') {
      const decision = value.response.response.hookSpecificOutput;
      assert.equal(decision.permissionDecision, 'deny');
      assert.match(decision.permissionDecisionReason, /mcp__minarbot__run_shell/);
      send({ type: 'control_request', request_id: 'workspace-loop', request: {
        subtype: 'can_use_tool', tool_name: 'Bash', input: { command: 'for file in *.test.mjs; do head -5 "$file"; done' },
      } });
    }
    if (value.type === 'control_response' && value.response.request_id === 'workspace-loop') {
      assert.equal(value.response.response.behavior, 'deny');
      assert.match(value.response.response.message, /mcp__minarbot__run_shell/);
      preToolUse('workspace-sandbox', 'mcp__minarbot__run_shell', workspaceShell);
    }
    if (value.type === 'control_response' && value.response.request_id === 'workspace-sandbox') {
      assert.equal(value.response.response.hookSpecificOutput.permissionDecision, 'allow');
      runWorkspaceShell();
      finish();
    }
    if (value.type === 'control_response' && value.response.request_id === approvalId) { const decision = value.response.response?.hookSpecificOutput?.permissionDecision; log({ decision }); finish(decision === 'allow'); }
  }
}

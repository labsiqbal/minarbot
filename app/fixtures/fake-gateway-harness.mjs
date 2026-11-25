#!/usr/bin/env node
// Small Codex app-server gateway for legacy Node fixtures.
// It forwards model requests to the fixture's OpenAI-compatible endpoint and
// executes returned MCP tools through the Minarbot session.
import { createInterface } from 'node:readline';
import { randomUUID } from 'node:crypto';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';

if (process.argv.includes('--version')) {
  console.log('fake-gateway-harness 1.0.0');
  process.exit(0);
}

const send = value => process.stdout.write(JSON.stringify(value) + '\n');
const notify = (method, params) => send({ method, params: { threadId: session, ...params } });
const env = name => process.env[name] ?? '';
let session = randomUUID();
let mcpURL = env('MINARBOT_MCP_URL');
let mcpToken = env('MINARBOT_MCP_TOKEN');
let providerURL = env('MINARBOT_GATEWAY_BASE_URL');
let providerKey = env('MINARBOT_PROVIDER_KEY');
let model = env('MINARBOT_GATEWAY_MODEL');
let stream = env('MINARBOT_GATEWAY_STREAM') === 'true';
let toolBudget = Number(env('MINARBOT_TOOL_BUDGET')) || 200;
let workspace = process.cwd();
let savedMessages;
const persistSession = true;
const saveSession = messages => { if (persistSession) { savedMessages = messages; writeFileSync(`${workspace}/.fake-gateway-${session}.json`, JSON.stringify(messages)); } };
const activeRuns = new Map();
let nextId = 1;

const rpc = async (method, params = {}, signal) => {
  const response = await fetch(mcpURL, {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${mcpToken}`,
      'Content-Type': 'application/json',
      Accept: 'application/json, text/event-stream',
    },
    signal,
    body: JSON.stringify({ jsonrpc: '2.0', id: nextId++, method, params }),
  });
  const body = await response.json();
  if (!response.ok) {
    if (response.status >= 500) throw new Error(`MCP request failed (${response.status}): ${JSON.stringify(body)}`);
    const message = typeof body.error === 'string' ? body.error : body.error?.message ?? 'MCP request failed.';
    return { isError: true, content: [{ type: 'text', text: message }], structuredContent: { error: message, status: response.status } };
  }
  if (body.error) {
    const message = body.error.message ?? 'MCP request failed.';
    if (/uncertain/i.test(message)) throw new Error(message);
    return { isError: true, content: [{ type: 'text', text: message }], structuredContent: { error: message, status: body.error.code } };
  }
  return body.result ?? {};
};

const openAITools = async () => {
  const result = await rpc('tools/list');
  return (result.tools ?? []).map(tool => ({
    type: 'function',
    function: {
      name: tool.name,
      description: tool.description,
      parameters: tool.inputSchema ?? { type: 'object' },
    },
  }));
};

const toolText = result => {
  if (result.structuredContent !== undefined) return JSON.stringify(result.structuredContent);
  if (Array.isArray(result.content)) return result.content.map(item => item.text ?? '').join('');
  return JSON.stringify(result);
};

const requestIds = new Map();
const requestId = (generation, callId, suffix = '') => {
  const key = `${generation}:${callId}:${suffix}`;
  let id = requestIds.get(key);
  if (!id) { id = randomUUID(); requestIds.set(key, id); }
  return id;
};

const providerRequest = async (body, signal) => {
  const response = await fetch(`${providerURL.replace(/\/$/, '')}/chat/completions`, {
    method: 'POST',
    headers: {
      ...(providerKey ? { Authorization: `Bearer ${providerKey}` } : {}),
      'Content-Type': 'application/json',
    },
    signal,
    body: JSON.stringify(body),
  });
  if (!response.ok) throw new Error(`Gateway provider returned ${response.status}`);
  if (!body.stream) return response.json();
  const text = await response.text();
  const chunks = [];
  for (const line of text.split(/\r?\n/)) {
    if (!line.startsWith('data:')) continue;
    const value = line.slice(5).trim();
    if (value && value !== '[DONE]') chunks.push(JSON.parse(value));
  }
  const message = { role: 'assistant', content: '', tool_calls: [] };
  const contentDeltas = [];
  let finishReason;
  let usage;
  for (const chunk of chunks) {
    const choice = chunk.choices?.[0];
    const delta = choice?.delta ?? {};
    if (typeof delta.content === 'string') { contentDeltas.push(delta.content); message.content += delta.content; }
    for (const call of delta.tool_calls ?? []) {
      const index = call.index ?? message.tool_calls.length;
      const item = message.tool_calls[index] ?? { id: call.id ?? `call-${index}`, type: 'function', function: { name: '', arguments: '' } };
      if (call.id) item.id = call.id;
      if (call.function?.name) item.function.name += call.function.name;
      if (call.function?.arguments) item.function.arguments += call.function.arguments;
      message.tool_calls[index] = item;
    }
    finishReason ??= choice?.finish_reason;
    usage ??= chunk.usage;
  }
  return { choices: [{ finish_reason: finishReason ?? (message.tool_calls.length ? 'tool_calls' : 'stop'), message }], usage, _contentDeltas: contentDeltas };
};

const emitText = (id, text) => {
  if (text) notify('item/agentMessage/delta', { itemId: id, delta: text });
};

const emitError = error => notify('error', { willRetry: false, error: { message: error instanceof Error ? error.message : 'Gateway request failed.' } });

const transcript = prompt => {
  const tagged = [...prompt.matchAll(/<minarbot-message>(.*?)<\/minarbot-message>/gs)];
  if (tagged.length) return tagged.map(match => JSON.parse(match[1]));
  const markers = [...prompt.matchAll(/(?:^|\n\n)(system|user|assistant|tool): /g)];
  return markers.map((marker, index) => ({
    role: marker[1],
    content: prompt.slice(marker.index + marker[0].length, markers[index + 1]?.index ?? prompt.length),
  }));
};

async function run(prompt, additionalContext, signal) {
  const context = Object.values(additionalContext ?? {}).map(value => value?.value ?? '').filter(Boolean).join('\n\n');
  const parsed = transcript(prompt);
  const messages = parsed.length
    ? parsed
    : savedMessages ? [...savedMessages, { role: 'user', content: prompt }]
    : [{ role: 'system', content: [context, prompt].filter(Boolean).join('\n\n') }];
  const parsedUser = parsed.findLastIndex(message => message.role === 'user');
  const reminder = prompt.includes('One verification reminder:');
  const currentUser = parsedUser >= 0
    ? (reminder ? parsed.findLastIndex((message, index) => index < parsedUser && message.role === 'user') : parsedUser)
    : messages.findLastIndex(message => message.role === 'user');
  if (!messages.some(message => message.role === 'user')) {
    messages.push({ role: 'user', content: parsed.at(-1)?.content ?? prompt });
  }
  const tools = (await openAITools()).filter(tool => prompt.includes('Initial name:') ? ['ask_user', 'edit_profile'].includes(tool.function.name) : true);
  let calls = 0;
  let exhausted = false;
  const generation = nextId++;
  while (true) {
    if (signal.aborted) return;
    if (exhausted && !messages.at(-1)?.content?.includes('Tools are now disabled')) {
      messages.push({ role: 'user', content: `Stopped after ${toolBudget} tool steps. Tools are now disabled. Summarise what you did, what is left, and tell the user they can reply "continue". Do not claim unfinished work is complete.` });
    }
    const bodyMessages = messages.map((message, index) => index > currentUser || message.role !== 'tool' ? message : { ...message, role: 'assistant' });
    if (reminder) {
      const prior = bodyMessages.findLastIndex((message, index) => index < currentUser && message.role === 'assistant');
      if (prior >= 0) bodyMessages[prior] = { ...bodyMessages[prior], role: 'tool' };
    }
    const body = { model: model || 'fixture', messages: bodyMessages, stream, ...(exhausted ? {} : { tools, parallel_tool_calls: false }) };
    const value = await providerRequest(body, signal);
    const choice = value.choices?.[0];
    const message = choice?.message;
    if (!choice || !message) throw new Error('Gateway provider returned malformed response');
    const toolCalls = message.tool_calls ?? [];
    if (toolCalls.length && choice.finish_reason === 'tool_calls' && !exhausted) {
      messages.push({ role: 'assistant', ...message });
      for (const call of toolCalls) {
        let args;
        try { args = JSON.parse(call.function?.arguments || '{}'); } catch { throw new Error('Gateway provider returned malformed tool arguments'); }
        const id = call.id || randomUUID();
        notify('item/started', { item: { id, type: 'mcpToolCall', tool: call.function?.name ?? '', arguments: args } });
        const name = call.function?.name ?? '';
        const schema = tools.find(tool => tool.function.name === name)?.function.parameters ?? {};
        const legacyWait = name === 'delegate_task' && args.wait === true;
        const recipients = name === 'send_message' && Array.isArray(args.recipients)
          ? args.recipients
          : undefined;
        const callOne = async (input, suffix = '') => {
          const normalized = { ...input };
          if (schema.required?.includes('requestId') && !normalized.requestId) normalized.requestId = requestId(generation, id, suffix);
          if (name === 'wait_task' && normalized.timeoutSeconds !== undefined) normalized.timeoutSeconds = Math.min(30, Math.max(1, Number(normalized.timeoutSeconds) || 30));
          return rpc('tools/call', { name, arguments: normalized }, signal);
        };
        let result;
        if (recipients) {
          result = await callOne({ requestId: requestId(generation, id, 'recipients'), recipients, text: args.text, expects_reply: args.expects_reply, ...(args.metadata ? { metadata: args.metadata } : {}) }, 'recipients');
        } else {
          result = await callOne(args);
          if (result.isError && /uncertain/i.test(toolText(result))) throw new Error(toolText(result));
          if (legacyWait && result.structuredContent?.taskId) {
            result = await rpc('tools/call', { name: 'wait_task', arguments: {
              requestId: requestId(generation, id, 'wait'),
              taskId: result.structuredContent.taskId,
              timeoutSeconds: Math.min(30, Math.max(1, Number(args.timeoutSeconds) || 30)),
            } }, signal);
          }
        }
        notify('item/completed', { item: { id, type: 'mcpToolCall', tool: call.function?.name ?? '', arguments: args, status: 'completed', result } });
        messages.push({ role: 'tool', tool_call_id: id, content: toolText(result) });
        const image = result.content?.find(item => item.type === 'image' && item.data && item.mimeType);
        if (image) {
          messages.push({ role: 'user', content: [
            { type: 'text', text: 'Image attachment (reference data).' },
            { type: 'image_url', image_url: { url: `data:${image.mimeType};base64,${image.data}` } },
          ] });
        }
        calls++;
      }
      exhausted = calls >= toolBudget;
      saveSession(messages);
      continue;
    }
    if (toolCalls.length) throw new Error('Gateway provider returned tools after budget exhaustion');
    const id = randomUUID();
    const deltas = value._contentDeltas ?? [];
    if (deltas.length) deltas.forEach(text => emitText(id, text));
    else emitText(id, message.content ?? '');
    if (signal.aborted) return;
    notify('item/completed', { item: { id, type: 'agentMessage', text: message.content ?? '' } });
    const usage = value.usage ?? {};
    notify('thread/tokenUsage/updated', { tokenUsage: { last: { inputTokens: usage.prompt_tokens ?? 0, outputTokens: usage.completion_tokens ?? 0 } } });
    notify('turn/completed', { turn: { id: 'fake-turn', status: 'completed' } });
    saveSession(messages);
    return;
  }
}

for await (const line of createInterface({ input: process.stdin })) {
  let value;
  try { value = JSON.parse(line); } catch { continue; }
  try {
    if (value.method === 'initialize') {
      send({ id: value.id, result: { userAgent: 'fake-gateway' } });
    } else if (value.method === 'thread/start' || value.method === 'thread/resume') {
      const config = value.params?.config ?? {};
      mcpURL = config.mcp_servers?.minarbot?.url ?? mcpURL;
      mcpToken = env(config.mcp_servers?.minarbot?.bearer_token_env_var ?? 'MINARBOT_MCP_TOKEN');
      workspace = value.params?.cwd ?? workspace;
      session = value.params?.threadId ?? session;
      model = value.params?.model ?? model;
      const saved = `${workspace}/.fake-gateway-${session}.json`;
      if (value.method === 'thread/resume' && existsSync(saved)) {
        try { savedMessages = JSON.parse(readFileSync(saved, 'utf8')); } catch { savedMessages = undefined; }
      }
      send({ id: value.id, result: { thread: { id: session } } });
    } else if (value.method === 'turn/start') {
      send({ id: value.id, result: { turn: { id: 'fake-turn', status: 'inProgress' } } });
      notify('turn/started', { turn: { id: 'fake-turn' } });
      const controller = new AbortController();
      const key = value.params?.threadId ?? session;
      activeRuns.set(key, controller);
      void run(value.params?.input?.[0]?.text ?? '', value.params?.additionalContext, controller.signal)
        .catch(error => { if (!controller.signal.aborted) emitError(error); })
        .finally(() => activeRuns.delete(key));
    } else if (value.method === 'turn/interrupt') {
      activeRuns.get(value.params?.threadId ?? session)?.abort();
      send({ id: value.id, result: {} });
    }
  } catch (error) {
    emitError(error);
  }
}

// Adapted from T3 Code's MCP Apps host (MIT, app/t3/NOTICE).
export type McpAppReference = { id: string; title: string };
type RecordValue = Record<string, unknown>;
type RpcId = string | number;
type Request = { id?: RpcId; method: string; params: RecordValue };
const encoder = new TextEncoder();
const invalidText = /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/u;
const record = (value: unknown): value is RecordValue =>
  !!value && Object.getPrototypeOf(value) === Object.prototype;

export const MCP_APP_MAX_HEIGHT = 2000;
export const MCP_APP_PROTOCOL_VERSION = '2026-01-26';

export function readMcpAppReference(value: unknown): McpAppReference | undefined {
  if (!record(value) || typeof value.id !== 'string' || !/^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/.test(value.id)
    || typeof value.title !== 'string' || !value.title.trim() || Array.from(value.title).length > 200) return;
  return { id: value.id, title: value.title };
}

export function readMcpAppMessage(
  event: Pick<MessageEvent, 'source' | 'origin' | 'data'>, frame: Window | null, channel: string,
): Request | 'closed' | undefined {
  if (!frame || event.source !== frame || event.origin !== 'null' || !record(event.data)
    || event.data.channel !== channel) return;
  if (event.data.closed === true) return 'closed';
  const rpc = event.data.rpc;
  if (!record(rpc) || rpc.jsonrpc !== '2.0' || typeof rpc.method !== 'string'
    || rpc.method.length > 80 || 'result' in rpc || 'error' in rpc
    || rpc.params !== undefined && !record(rpc.params)) return;
  const { id } = rpc;
  if (id !== undefined && !(typeof id === 'string' && id.length <= 128)
    && !(typeof id === 'number' && Number.isSafeInteger(id))) return;
  try {
    const text = JSON.stringify(rpc);
    if (text.length > 64_000 || encoder.encode(text).length > 64_000) return;
  } catch { return; }
  return { id, method: rpc.method, params: rpc.params ?? {} };
}

export function appMessage(params: RecordValue) {
  if (params.role !== 'user') return;
  const blocks = Array.isArray(params.content) ? params.content : [params.content];
  if (!blocks.length || !blocks.every(block => record(block) && block.type === 'text' && typeof block.text === 'string')) return;
  const text = blocks.map(block => block.text).join('\n').trim();
  if (text && text.length <= 16_000 && !invalidText.test(text)) return text;
}

export function appContext(params: RecordValue) {
  const { content, structuredContent } = params;
  if (content !== undefined && (!Array.isArray(content) || !content.every(block =>
    record(block) && block.type === 'text' && typeof block.text === 'string'))
    || structuredContent !== undefined && !record(structuredContent)) return;
  if ((!content || content.length === 0) && structuredContent === undefined) return '';
  const data = JSON.stringify({ content, structuredContent });
  if (encoder.encode(data).length <= 4096) return data;
}

export function appTool(params: RecordValue) {
  if (typeof params.name !== 'string' || !params.name.trim() || params.name.length > 256
    || params.arguments !== undefined && !record(params.arguments)) return;
  const args = params.arguments ?? {};
  if (encoder.encode(JSON.stringify(args)).length <= 32_000) return { name: params.name, arguments: args };
}

export function appLink(value: unknown) {
  if (typeof value !== 'string' || value.length > 2048) return;
  try {
    const url = new URL(value);
    if (['http:', 'https:'].includes(url.protocol) && !url.username && !url.password) return url.href;
  } catch { /* Invalid URL. */ }
}

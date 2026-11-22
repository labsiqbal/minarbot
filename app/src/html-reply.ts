// Adapted from T3 Code (MIT, app/t3/NOTICE). See app/t3/provenance.json.
const uuid = /^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/;
const maxContextBytes = 4096;

export type HtmlReplyReference = { id: string; title: string; height: number };
export type HtmlReplyContext = { renderId: string; data: string };
export type HtmlReplyAction = { kind: 'draft'; text: string } | { kind: 'context'; context: HtmlReplyContext };

export function validHtmlReplyContext(value: unknown): value is HtmlReplyContext {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const { renderId, data } = value as Partial<HtmlReplyContext>;
  if (Object.keys(value).length !== 2 || typeof renderId !== 'string' || !uuid.test(renderId)
    || typeof data !== 'string' || data.length > maxContextBytes || new TextEncoder().encode(data).length > maxContextBytes) return false;
  try { JSON.parse(data); return true; } catch { return false; }
}

export function readHtmlReply(value: unknown): HtmlReplyReference | undefined {
  if (!value || typeof value !== 'object') return;
  const { id, title, height } = value as Partial<HtmlReplyReference>;
  if (typeof id !== 'string' || !uuid.test(id) || typeof title !== 'string' || !title.trim()
    || Array.from(title).length > 200 || typeof height !== 'number' || !Number.isInteger(height)
    || height < 80 || height > 2000) return;
  return { id, title, height };
}

export function readHtmlReplyMessage(event: Pick<MessageEvent, 'source' | 'origin' | 'data'>, frame: Window | null, channel: string, renderId: string) {
  if (!frame || event.source !== frame || event.origin !== 'null') return;
  const message = event.data;
  if (!message || typeof message !== 'object' || Array.isArray(message) || message.channel !== channel) return;
  const { method, value } = message;
  switch (method) {
    case 'ready': return { kind: 'ready' } as const;
    case 'size':
      if (typeof value === 'number' && Number.isFinite(value) && value > 0) return { kind: 'size', height: Math.min(20_000, Math.max(80, Math.ceil(value))) } as const;
      return;
    case 'draft':
      if (typeof value === 'string' && value.length <= 16000 && value.trim() && !/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/.test(value)) return { kind: 'draft', text: value.trim() } as const;
      return;
    case 'context': {
      const context = { renderId, data: value };
      if (validHtmlReplyContext(context)) return { kind: 'context', context } as const;
      return;
    }
    case 'link':
      if (typeof value !== 'string' || value.length > 2048 || !/^https?:\/\//i.test(value)) return;
      try {
        const url = new URL(value);
        if (!url.username && !url.password) return { kind: 'link', href: url.href } as const;
      } catch { /* URL is malformed. */ }
  }
}

import { DatabaseSync } from 'node:sqlite';
import { ProtocolError } from './shared/protocol.ts';
import type WebSocket from 'ws';
import type { ClientOptions } from 'ws';

type KeyEntry = { getPassword(): string | null; setPassword(value: string): void };
export type HubLinkOptions = {
  fetch?: typeof fetch;
  webSocket?: (url: string, options: ClientOptions) => WebSocket;
  receive?: (frame: string, socket: WebSocket) => string[];
  origin: string;
  hubSigningPublicKey: string;
  development?: boolean;
  entry?: (service: string, name: string) => KeyEntry;
  reconnectMs?: number;
  now?: () => number;
};

export function hubTestTransport(options?: HubLinkOptions) {
  const sockets = new Map<string, { socket: WebSocket; events: string[]; closed: boolean }>();
  return {
    async call(operation: string, args: Record<string, unknown>): Promise<unknown> {
      if (!options) throw new Error('Hub fixture is unavailable.');
      if (operation === 'hub-clock') return options.now?.() ?? Date.now();
      if (operation === 'hub-key-get' || operation === 'hub-key-set') {
        if (!options.entry) throw new Error('Hub fixture requires isolated key entries.');
        const entry = options.entry(String(args.service), String(args.name));
        if (operation === 'hub-key-set') { entry.setPassword(String(args.value)); return null; }
        return entry.getPassword();
      }
      if (operation === 'hub-http') {
        const response = await (options.fetch ?? fetch)(options.origin + String(args.path), { method: 'POST', redirect: 'error', headers: { 'Content-Type': 'application/json' }, body: String(args.body), signal: AbortSignal.timeout(10000) });
        const body = await response.text(); if (Buffer.byteLength(body) > 65536) throw new Error('Hub fixture response exceeded.');
        return { status: response.status, body };
      }
      if (operation === 'hub-connect') {
        if (!options.webSocket) throw new Error('Hub fixture requires isolated WebSocket transport.');
        const socket = options.webSocket(options.origin.replace(/^http/, 'ws') + '/node', { headers: args.headers as Record<string, string>, maxPayload: 65536, perMessageDeflate: false, followRedirects: false, handshakeTimeout: 10000 });
        const entry = { socket, events: [] as string[], closed: false }; sockets.set(String(args.id), entry);
        socket.on('message', (data, binary) => {
          if (binary) { socket.terminate(); return; }
          const frame = data.toString(), frames = options.receive?.(frame, socket) ?? [frame];
          if (entry.events.length + frames.length > 128) { socket.terminate(); return; }
          entry.events.push(...frames);
        });
        socket.on('close', () => { entry.closed = true; }); socket.on('error', () => { socket.terminate(); });
        return null;
      }
      const entry = sockets.get(String(args.id)); if (!entry) throw new Error('Hub fixture socket missing.');
      if (operation === 'hub-poll') return { frames: entry.events.splice(0), closed: entry.closed };
      if (operation === 'hub-send') { if (entry.socket.readyState !== 1 || entry.socket.bufferedAmount > 1048576) throw new Error('Hub fixture unavailable.'); entry.socket.send(String(args.frame)); return null; }
      if (operation === 'hub-close') {
        if (!entry.closed) await new Promise<void>(resolve => {
          const timer = setTimeout(() => { entry.socket.terminate(); resolve(); }, 1000);
          entry.socket.once('close', () => { clearTimeout(timer); resolve(); }); entry.socket.close();
        });
        sockets.delete(String(args.id)); return null;
      }
      throw new Error('Unknown hub fixture operation.');
    },
    close() { for (const entry of sockets.values()) entry.socket.terminate(); sockets.clear(); },
  };
}
export function rustHubFacade(database: string, origin: string, token: string) {
  const read = <T>(work: (db: DatabaseSync) => T) => { const db = new DatabaseSync(database, { readOnly: true }); try { return work(db); } finally { db.close(); } };
  const snapshot = () => read(db => JSON.parse(String(db.prepare('SELECT value FROM hub_link_runtime WHERE id=1').get()?.value ?? '{}')));
  const command = async (operation: string, value: unknown = {}) => {
    const response = await fetch(origin + '/api/hub-link', { method: 'POST', headers: { Origin: origin, 'Content-Type': 'application/json', 'X-Minarbot-Test-Hub': token }, body: JSON.stringify({ operation, value }) });
    const result = await response.json(); if (!response.ok) throw new ProtocolError(response.status, result.code ?? 'local_policy_denied', result.error);
    return result;
  };
  return {
    status: () => snapshot().status,
    approvals: () => snapshot().approvals ?? [],
    requests: () => read(db => db.prepare('SELECT value FROM hub_link_requests LIMIT 100').all().map(row => JSON.parse(String(row.value)))),
    localPolicyDigest: (agent: string) => snapshot().policies[agent],
    check: (turn: string) => { const saved = read(db => db.prepare('SELECT state,frame FROM hub_link_turns WHERE turn_id=?').get(turn)); if (!saved) return undefined; if (saved.state !== 'admitted' || !snapshot().status.connected) throw new ProtocolError(403, 'local_policy_denied', 'Local owner policy does not allow this work.'); return JSON.parse(String(saved.frame)); },
    beforeEffect: (turn: string, kind: string, payload: unknown) => command('beforeEffect', { turn, kind, payload }),
    close: () => command('close'),
  };
}

import { randomBytes } from 'node:crypto';
import type { IncomingMessage } from 'node:http';
import { closedObject, ProtocolError, sha256 } from '../shared/protocol.ts';
import { transaction } from './schema.ts';
import { singleHeader, type HostGateway } from './host-gateway.ts';
import type { HubAuthority, IdentityVerifier, VerifiedIdentity } from './project-authority.ts';

export type SessionIdentity = VerifiedIdentity & { sessionId: string; expiresAt: number };
export interface SessionVerifier extends IdentityVerifier { verify(token: string): Promise<SessionIdentity> }
const invalid = () => new ProtocolError(401, 'session_invalid', 'Session is invalid or expired.');
const forbidden = () => new ProtocolError(403, 'origin_or_csrf_denied', 'Origin or CSRF proof is not allowed.');
const opaque = () => randomBytes(32).toString('base64url');
function proof(value: unknown): string {
  if (typeof value !== 'string' || !/^[A-Za-z0-9_-]{43}$/.test(value)) throw invalid();
  return value;
}
/** Explicit local/dev fixture tokens. No subject supplied by an HTTP client can mint an account. */
export function createStubVerifier(tokens: ReadonlyMap<string, SessionIdentity>): SessionVerifier {
  const fixtures = new Map([...tokens].map(([token, identity]) => [token, structuredClone(identity)]));
  return { async verify(token) { const value = fixtures.get(token); if (!value) throw invalid(); return structuredClone(value); } };
}

export function browserCookie(request: IncomingMessage, name: string) {
  const entries = (singleHeader(request, 'cookie') ?? '').split(';').map(entry => entry.trim()).filter(entry => entry.startsWith(`${name}=`));
  if (entries.length !== 1) throw invalid();
  return proof(entries[0].slice(name.length + 1));
}
export function cookie(name: '__Host-session' | '__Host-login', value: string, seconds: number) {
  return `${name}=${value}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=${Math.max(0, seconds)}`;
}
export function requireOrigin(request: IncomingMessage, host: string) {
  if (singleHeader(request, 'origin') !== `https://${host}`) throw forbidden();
}

export function createHubSessions(authority: HubAuthority, gateway: HostGateway, verifier: SessionVerifier) {
  const { db, now } = authority;
  // Dedicated extension tables; P1-A's checksummed authority migration remains immutable.
  transaction(db, () => db.exec(`
    CREATE TABLE IF NOT EXISTS hub_browser_sessions (
      digest TEXT PRIMARY KEY, host TEXT NOT NULL, project_id TEXT, account_id TEXT NOT NULL REFERENCES accounts(id),
      identity_json TEXT NOT NULL, csrf TEXT NOT NULL, expires_at INTEGER NOT NULL,
      parent_digest TEXT, generation TEXT NOT NULL
    ) STRICT;
    CREATE TABLE IF NOT EXISTS hub_browser_starts (
      state_digest TEXT PRIMARY KEY, browser_digest TEXT NOT NULL, host TEXT NOT NULL, project_id TEXT NOT NULL,
      generation TEXT NOT NULL, expires_at INTEGER NOT NULL
    ) STRICT;
    CREATE TABLE IF NOT EXISTS hub_browser_codes (
      digest TEXT PRIMARY KEY, state_digest TEXT NOT NULL UNIQUE REFERENCES hub_browser_starts(state_digest) ON DELETE CASCADE,
      parent_digest TEXT NOT NULL REFERENCES hub_browser_sessions(digest) ON DELETE CASCADE, expires_at INTEGER NOT NULL
    ) STRICT;
    CREATE TABLE IF NOT EXISTS hub_browser_denials (issuer TEXT NOT NULL, sid TEXT NOT NULL, PRIMARY KEY(issuer,sid)) STRICT;
  `));
  type Session = { digest: string; host: string; projectId?: string; accountId: string; identity: SessionIdentity; csrf: string; expiresAt: number; parent?: string; generation: string };
  function current(session: Session): Session {
    const identity = session.identity;
    if (session.expiresAt <= now() || identity.expiresAt <= now() || session.generation !== authority.generation() || db.prepare('SELECT 1 FROM hub_browser_denials WHERE issuer=? AND sid=?').get(identity.issuer, identity.sessionId)) throw invalid();
    const account = authority.account(session.accountId);
    if (account.issuer !== identity.issuer || account.subject !== identity.subject) throw invalid();
    if (session.parent) read(session.parent, gateway.accountsHost);
    if (session.projectId) gateway.project(session.host, session.accountId, session.projectId);
    return session;
  }
  function read(digest: string, host: string): Session {
    const row = db.prepare('SELECT * FROM hub_browser_sessions WHERE digest=? AND host=?').get(digest, host);
    if (!row) throw invalid();
    return current({ digest, host, projectId: row.project_id === null ? undefined : String(row.project_id), accountId: String(row.account_id), identity: JSON.parse(String(row.identity_json)), csrf: String(row.csrf), expiresAt: Number(row.expires_at), parent: row.parent_digest === null ? undefined : String(row.parent_digest), generation: String(row.generation) });
  }
  function get(request: IncomingMessage, host: string, mutation = false) {
    const session = read(sha256(browserCookie(request, '__Host-session')), host);
    if (mutation) {
      requireOrigin(request, host);
      if (singleHeader(request, 'x-csrf-token') !== session.csrf) throw forbidden();
    }
    return session;
  }
  function mint(host: string, accountId: string, identity: SessionIdentity, projectId?: string, parent?: Session) {
    const value = opaque(), csrf = opaque(), expiresAt = Math.min(identity.expiresAt, now() + 900000, parent?.expiresAt ?? Infinity);
    if (expiresAt <= now()) throw invalid();
    db.prepare('INSERT INTO hub_browser_sessions VALUES(?,?,?,?,?,?,?,?,?)').run(sha256(value), host, projectId ?? null, accountId, JSON.stringify(identity), csrf, expiresAt, parent?.digest ?? null, authority.generation());
    return { csrf, expiresAt, cookie: cookie('__Host-session', value, Math.floor((expiresAt - now()) / 1000)) };
  }
  function remove(digest: string) {
    db.prepare('DELETE FROM hub_browser_sessions WHERE parent_digest=? OR digest=?').run(digest, digest);
  }
  function rotate(request: IncomingMessage, host: string) {
    try { const old = read(sha256(browserCookie(request, '__Host-session')), host); remove(old.digest); }
    catch (error) { if (!(error instanceof ProtocolError)) throw error; }
  }
  return {
    get,
    prune() {
      db.prepare('DELETE FROM hub_browser_codes WHERE expires_at<=?').run(now());
      db.prepare('DELETE FROM hub_browser_starts WHERE expires_at<=? OR generation!=?').run(now(), authority.generation());
      db.prepare('DELETE FROM hub_browser_sessions WHERE expires_at<=? OR generation!=?').run(now(), authority.generation());
    },
    start(host: string) {
      const project = authority.one("SELECT p.id FROM host_bindings h JOIN projects p ON p.id=h.project_id WHERE h.host=? AND h.active=1 AND p.status='active'", gateway.registered(host));
      if (!project || host === gateway.accountsHost) throw invalid();
      const state = opaque(), browser = opaque();
      db.prepare('INSERT INTO hub_browser_starts VALUES(?,?,?,?,?,?)').run(sha256(state), sha256(browser), host, project.id, authority.generation(), now() + 300000);
      return { state, login: `https://${gateway.accountsHost}/login?state=${state}`, cookie: cookie('__Host-login', browser, 300) };
    },
    async login(request: IncomingMessage, host: string) {
      if (host !== gateway.accountsHost) throw invalid();
      requireOrigin(request, host);
      // Bootstrap CSRF is the central browser-bound nonce, not an ambient Clerk cookie.
      if (singleHeader(request, 'x-csrf-token') !== browserCookie(request, '__Host-login')) throw forbidden();
      const authorization = singleHeader(request, 'authorization') ?? '';
      if (!/^Bearer [^\s,]+$/.test(authorization)) throw invalid();
      const identity = await verifier.verify(authorization.slice(7));
      const { accountId } = await authority.authenticate({ verify: async () => identity }, authorization.slice(7));
      if (!identity.sessionId || !Number.isSafeInteger(identity.expiresAt) || identity.expiresAt <= now()) throw invalid();
      return transaction(db, () => {
        if (db.prepare('SELECT 1 FROM hub_browser_denials WHERE issuer=? AND sid=?').get(identity.issuer, identity.sessionId)) throw invalid();
        authority.account(accountId); rotate(request, host);
        return mint(host, accountId, identity);
      });
    },
    bootstrap() {
      const nonce = opaque();
      return { csrf: nonce, cookie: cookie('__Host-login', nonce, 300) };
    },
    handoff(request: IncomingMessage, host: string, state: unknown) {
      const parent = get(request, host, true);
      if (host !== gateway.accountsHost) throw invalid();
      return transaction(db, () => {
        const start = db.prepare('SELECT * FROM hub_browser_starts WHERE state_digest=?').get(sha256(proof(state)));
        if (!start || Number(start.expires_at) <= now() || start.generation !== authority.generation()) throw invalid();
        gateway.project(String(start.host), parent.accountId, String(start.project_id));
        const value = opaque();
        db.prepare('INSERT INTO hub_browser_codes VALUES(?,?,?,?) ON CONFLICT(state_digest) DO UPDATE SET digest=excluded.digest,parent_digest=excluded.parent_digest,expires_at=excluded.expires_at').run(sha256(value), start.state_digest, parent.digest, Math.min(now() + 60000, parent.expiresAt));
        return { callback: `https://${start.host}/auth/callback#code=${value}&state=${state}`, expiresAt: Math.min(now() + 60000, parent.expiresAt) };
      });
    },
    redeem(request: IncomingMessage, host: string, input: unknown) {
      requireOrigin(request, host);
      const data = closedObject(input, ['state', 'code']), state = proof(data.state);
      if (singleHeader(request, 'x-csrf-token') !== state) throw forbidden();
      return transaction(db, () => {
        const row = db.prepare('SELECT s.*,c.digest,c.parent_digest,c.expires_at AS code_expires_at FROM hub_browser_starts s JOIN hub_browser_codes c ON c.state_digest=s.state_digest WHERE c.digest=? AND s.state_digest=?').get(sha256(proof(data.code)), sha256(state));
        if (!row || row.host !== host || Number(row.expires_at) <= now() || Number(row.code_expires_at) <= now() || row.generation !== authority.generation() || row.browser_digest !== sha256(browserCookie(request, '__Host-login'))) throw invalid();
        const parent = read(String(row.parent_digest), gateway.accountsHost);
        gateway.project(host, parent.accountId, String(row.project_id));
        rotate(request, host);
        const result = mint(host, parent.accountId, parent.identity, String(row.project_id), parent);
        db.prepare('DELETE FROM hub_browser_starts WHERE state_digest=?').run(row.state_digest);
        return result;
      });
    },
    logout(request: IncomingMessage, host: string, global: boolean) {
      const actor = get(request, host, true);
      transaction(db, () => {
        if (global) {
          db.prepare('INSERT OR IGNORE INTO hub_browser_denials VALUES(?,?)').run(actor.identity.issuer, actor.identity.sessionId);
          const rows = db.prepare('SELECT digest,identity_json FROM hub_browser_sessions').all();
          for (const row of rows) { const identity = JSON.parse(String(row.identity_json)); if (identity.issuer === actor.identity.issuer && identity.sessionId === actor.identity.sessionId) remove(String(row.digest)); }
        } else remove(actor.digest);
      });
      return { state: global ? 'upstream_pending' : 'host_revoked' };
    },
  };
}
export type HubSessions = ReturnType<typeof createHubSessions>;

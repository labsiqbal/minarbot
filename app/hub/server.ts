import { createPrivateKey } from 'node:crypto';
import { hubAssets, hubLeptosAssets, hubDocument, createHubWebApi } from './web.ts';
import { createServer, type IncomingMessage, type ServerResponse } from 'node:http';
import type { Duplex } from 'node:stream';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { closedObject, id, parseJson, ProtocolError, sha256 } from '../shared/protocol.ts';
import { createHostGateway, notFound, singleHeader } from './host-gateway.ts';
import { createHubSessions, createClerkVerifier, createStubVerifier, cookie, requireOrigin, type SessionIdentity, type SessionVerifier } from './sessions.ts';
import { createHubAuthority, type HubAuthority } from './project-authority.ts';
import { createHubRouting, type HubRouting, type PostMessage } from './routing.ts';
import { openHubDatabase } from './schema.ts';
import { attachNodeLink, type NodeLinkDeps } from './node-link.ts';

export type NodeUpgradeHandler = (request: IncomingMessage, socket: Duplex, head: Buffer,
  context: Readonly<{ host: string; authority: HubAuthority; routing: HubRouting }>) => void | Promise<void>;

const callbackScript = `const data=new URLSearchParams(location.hash.slice(1));history.replaceState(null,'','/auth/callback');const state=data.get('state'),code=data.get('code');fetch('/auth/redeem',{method:'POST',headers:{'Content-Type':'application/json','X-CSRF-Token':state||''},body:JSON.stringify({state,code})}).then(async r=>{if(!r.ok)throw Error();await r.json();const target=sessionStorage.getItem('minarbot:hub-return');sessionStorage.removeItem('minarbot:hub-return');location.replace(target==='/invite'?'/invite':'/');}).catch(()=>{document.body.textContent='Sign-in expired. Return to the project and sign in again.';});`;
const callbackCsp = `default-src 'none'; script-src 'sha256-${Buffer.from(sha256(callbackScript), 'hex').toString('base64')}'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'`;
const callbackHtml = `<!doctype html><html lang="en"><meta charset="utf-8"><title>Minarbot sign-in</title><body>Completing sign-in...<script>${callbackScript}</script></body></html>`;

/** Separate HTTP composition. Never imports or mounts the owner application. Does not listen by itself. */
export function createHubServer(options: {
  authority: HubAuthority; verifier: SessionVerifier; accountsHost: string; domain: string;
  nodeLink?: Omit<NodeLinkDeps, 'authority' | 'routing'>;
  trustedGatewayAddresses?: string[]; pollIntervalMs?: number; maxStreams?: number;
  web?: { distPath: string; developmentIdentity?: boolean };
}) {
  const { authority } = options, routing = createHubRouting(authority);
  const gateway = createHostGateway(authority, options), sessions = createHubSessions(authority, gateway, options.verifier);
  if (options.nodeLink && options.nodeLink.origin !== `https://${gateway.accountsHost}`) throw new Error('Node origin must match accounts host.');
  const streams = new Set<{ close(): void; tick(): void }>(), upgrades = new Set<Duplex>();
  let upgradeHandler: NodeUpgradeHandler | undefined;
  let nodeLink: ReturnType<typeof attachNodeLink> | undefined;
  const web = options.web ? hubAssets(options.web.distPath) : undefined, browser = createHubWebApi(authority);
  const leptos = options.web && process.env.MINARBOT_APPEARANCE_CLIENT === 'leptos' ? hubLeptosAssets(options.web.distPath) : undefined;
  const rate = new Map<string, { count: number; until: number }>();
  function limit(key: string, ceiling: number) {
    const prior = rate.get(key), row = prior && prior.until > authority.now() ? prior : { count: 0, until: authority.now() + 60000 };
    if (++row.count > ceiling || !rate.has(key) && rate.size >= 10000) throw new ProtocolError(429, 'rate_limited', 'Retry later.', true);
    rate.set(key, row);
  }
  function security(response: ServerResponse) {
    response.setHeader('Cache-Control', 'no-store');
    response.setHeader('Referrer-Policy', 'no-referrer');
    response.setHeader('X-Content-Type-Options', 'nosniff');
    response.setHeader('Content-Security-Policy', "default-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'");
  }
  function reply(response: ServerResponse, value: unknown, cookies?: string[]) {
    if (cookies) response.setHeader('Set-Cookie', cookies);
    response.setHeader('Content-Type', 'application/json; charset=utf-8');
    response.end(JSON.stringify(value));
  }
  async function body(request: IncomingMessage) {
    if (!/^application\/json(?:\s*;\s*charset=utf-8)?$/i.test(singleHeader(request, 'content-type') ?? '')) throw new ProtocolError(415, 'unsupported_media_type', 'Use Content-Type application/json.');
    if (Number(singleHeader(request, 'content-length') ?? 0) > 65536) throw new ProtocolError(413, 'payload_too_large', 'JSON payload exceeds the limit.');
    const chunks: Buffer[] = []; let size = 0;
    for await (const chunk of request.iterator({ destroyOnReturn: false })) {
      size += chunk.length;
      if (size > 65536) throw new ProtocolError(413, 'payload_too_large', 'JSON payload exceeds the limit.');
      chunks.push(chunk);
    }
    return parseJson(Buffer.concat(chunks).toString('utf8'), 65536);
  }
  function subscribe(request: IncomingMessage, response: ServerResponse, host: string, sectionId?: string) {
    const actor = sessions.get(request, host), ctx = gateway.project(host, actor.accountId, actor.projectId);
    authority.member(ctx); if (sectionId) authority.section(ctx, sectionId);
    if (streams.size >= (options.maxStreams ?? 100)) throw new ProtocolError(429, 'stream_limit', 'Retry later.', true);
    response.setHeader('Content-Type', 'text/event-stream'); response.setHeader('X-Accel-Buffering', 'no'); response.flushHeaders();
    let cursor = 0, snapshot = '', turnSnapshot = '', heartbeat = 0, closed = false;
    const stream = {
      close() { if (closed) return; closed = true; clearTimeout(expiry); streams.delete(stream); response.end(); },
      tick() {
        if (closed) return;
        try {
          sessions.get(request, host); authority.member(ctx); if (sectionId) authority.section(ctx, sectionId);
          for (const event of authority.authorityEvents(ctx, cursor)) {
            sessions.get(request, host); authority.member(ctx);
            if (!response.write(`id: ${event.cursor}\nevent: authority\ndata: ${JSON.stringify(event)}\n\n`)) { stream.close(); return; }
            cursor = Number(event.cursor);
          }
          if (sectionId) {
            const history = routing.history(ctx, sectionId), serialized = JSON.stringify(history), digest = sha256(serialized);
            if (digest !== snapshot) {
              sessions.get(request, host); authority.section(ctx, sectionId);
              if (!response.write(`event: section.snapshot\ndata: ${serialized}\n\n`)) { stream.close(); return; }
              snapshot = digest;
            }
          }
          if (sectionId) {
            const turns = JSON.stringify(browser.turns(ctx, sectionId));
            if (turns !== turnSnapshot) {
              sessions.get(request, host); authority.section(ctx, sectionId);
              if (!response.write(`event: turn.snapshot\ndata: ${turns}\n\n`)) { stream.close(); return; }
              turnSnapshot = turns;
            }
          }
          if (authority.now() - heartbeat >= 15000) { heartbeat = authority.now(); if (!response.write(': heartbeat\n\n')) stream.close(); }
        } catch { stream.close(); }
      },
    };
    const expiry = setTimeout(() => stream.close(), Math.max(1, Math.min(900000, actor.expiresAt - authority.now())));
    expiry.unref(); streams.add(stream); response.on('close', () => stream.close()); stream.tick();
  }
  const server = createServer({ maxHeaderSize: 16384, requestTimeout: 15000, headersTimeout: 10000 }, async (request, response) => {
    security(response);
    try {
      const host = gateway.resolve(request), raw = request.url ?? '';
      if (host === gateway.accountsHost && nodeLink && await nodeLink.handleHttp(request, response)) return;
      if (!raw.startsWith('/') || raw.startsWith('//') || raw.includes('#') || /[\\\u0000-\u0020]/.test(raw)) throw notFound();
      const url = new URL(raw, `https://${host}`), path = url.pathname, central = host === gateway.accountsHost;
      if (path !== raw.split('?')[0]) throw notFound();
      const route = `${request.method} ${path}`;
      if (web && request.method === 'GET' && (['/', '/login', '/invite', '/join', '/nodes/link'].includes(path) || web.files.has(path) || leptos?.files.has(path))) {
        if (singleHeader(request, 'origin') !== undefined) requireOrigin(request, host);
        const asset = web.files.get(path) ?? leptos?.files.get(path);
        if (asset) {
          const extension = path.split('.').at(-1)!;
          const types: Record<string, string> = { js: 'text/javascript', css: 'text/css', woff2: 'font/woff2', svg: 'image/svg+xml', wasm: 'application/wasm' };
          response.setHeader('Content-Type', types[extension] ?? 'application/octet-stream');
          if (leptos?.files.has(path)) {
            response.setHeader('Cache-Control', 'public, max-age=31536000, immutable');
            const compressed = leptos.gzip.get(path);
            if (compressed) {
              response.setHeader('Vary', 'Accept-Encoding');
              const acceptsGzip = (singleHeader(request, 'accept-encoding') ?? '').split(',').some(item => {
                const [encoding, ...params] = item.split(';').map(part => part.trim().toLowerCase());
                return encoding === 'gzip' && !params.some(param => param.startsWith('q=') && !(Number(param.slice(2)) > 0));
              });
              if (acceptsGzip) {
                response.setHeader('Content-Encoding', 'gzip');
                response.end(compressed); return;
              }
            }
          }
          response.end(asset); return;
        }
        if (!central && ['/login', '/join', '/nodes/link'].includes(path) || central && path === '/invite') throw notFound();
        const client = leptos && url.searchParams.get('ui') !== 'react' ? leptos : web;
        url.searchParams.set('ui', 'react');
        const fallback = (path + url.search).replaceAll('&', '&amp;');
        const wasmSource = client === leptos ? " 'wasm-unsafe-eval'" : '';
        const page = hubDocument(client.html.replace('__REACT_HUB__', fallback), { accountsHost: gateway.accountsHost, domain: options.domain, developmentIdentity: !!options.web?.developmentIdentity });
        response.setHeader('Content-Security-Policy', `default-src 'none'; script-src 'self' 'nonce-${page.nonce}'${wasmSource}; style-src 'self' 'nonce-${page.nonce}'; font-src 'self'; img-src 'self' data:; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'`);
        response.setHeader('Content-Type', 'text/html; charset=utf-8'); response.end(page.html); return;
      }
      // Explicit browser allowlist. No local-owner API or proxy is mounted.
      const publicRoutes = new Set(central ? ['GET /login', 'GET /auth/bootstrap', 'POST /auth/session', 'POST /auth/handoff'] : ['GET /auth/start', 'GET /auth/callback', 'POST /auth/redeem']);
      const privateRoutes = new Set(central ? ['GET /api/session', 'POST /auth/logout', 'GET /api/projects', 'POST /api/projects', 'GET /api/nodes', ...(nodeLink ? ['GET /api/nodes/enrollment', 'POST /api/nodes/enrollment/approve'] : []), 'POST /api/agents', 'POST /api/participations', 'POST /api/project-options'] : ['GET /api/session', 'POST /auth/logout', 'GET /api/project', 'GET /api/sections', 'POST /api/sections', 'GET /api/history', 'POST /api/messages', 'GET /api/turn', 'GET /api/events', 'GET /api/invitation-session', 'GET /api/members', 'POST /api/members', 'GET /api/invitations', 'POST /api/invitations', 'POST /api/invitations/revoke', 'POST /api/invitations/accept', 'GET /api/participants', 'POST /api/participants/consent', 'POST /api/participants/withdraw', 'GET /api/turns']);
      if (!publicRoutes.has(route) && !privateRoutes.has(route)) throw notFound();
      if (singleHeader(request, 'origin') !== undefined) requireOrigin(request, host);
      if (request.method === 'POST') requireOrigin(request, host);
      if (route === 'GET /auth/bootstrap' || route === 'GET /login') {
        limit(`bootstrap:${request.socket.remoteAddress}`, 60);
        const result = sessions.bootstrap(); reply(response, { csrf: result.csrf, identity: 'central', state: url.searchParams.get('state') }, [result.cookie]); return;
      }
      if (route === 'GET /auth/start') {
        limit(`start:${request.socket.remoteAddress}`, 60);
        const result = sessions.start(host); reply(response, { state: result.state, login: result.login }, [result.cookie]); return;
      }
      if (route === 'GET /auth/callback') {
        response.setHeader('Content-Security-Policy', callbackCsp); response.setHeader('Content-Type', 'text/html; charset=utf-8'); response.end(callbackHtml); return;
      }
      if (route === 'POST /auth/session') {
        limit(`login:${request.socket.remoteAddress}`, 60); closedObject(await body(request), []);
        const result = await sessions.login(request, host);
        reply(response, { csrf: result.csrf, expiresAt: result.expiresAt }, [result.cookie, cookie('__Host-login', '', 0)]); sweep(); return;
      }
      if (route === 'POST /auth/redeem') {
        limit(`redeem:${request.socket.remoteAddress}`, 60);
        const result = sessions.redeem(request, host, await body(request));
        reply(response, { csrf: result.csrf, expiresAt: result.expiresAt }, [result.cookie, cookie('__Host-login', '', 0)]); sweep(); return;
      }
      const actor = sessions.get(request, host, request.method === 'POST');
      if (route === 'POST /auth/handoff') {
        const data = closedObject(await body(request), ['state']); reply(response, sessions.handoff(request, host, data.state)); return;
      }
      if (route === 'POST /auth/logout') {
        const data = closedObject(await body(request), ['global']);
        if (typeof data.global !== 'boolean') throw new ProtocolError(400, 'invalid_logout', 'Choose host or global logout.');
        const result = sessions.logout(request, host, data.global); sweep(); reply(response, result, [cookie('__Host-session', '', 0), cookie('__Host-login', '', 0)]); return;
      }
      if (central) {
        if (nodeLink && route === 'GET /api/nodes/enrollment') {
          if (url.searchParams.size !== 1 || !url.searchParams.has('humanCode')) throw notFound();
          reply(response, nodeLink.inspectEnrollment(actor.accountId, url.searchParams.get('humanCode')!)); return;
        }
        if (nodeLink && route === 'POST /api/nodes/enrollment/approve') {
          const data = closedObject(await body(request), ['humanCode', 'fingerprint']);
          const current = sessions.get(request, host, true);
          if (typeof data.humanCode !== 'string' || nodeLink.inspectEnrollment(current.accountId, data.humanCode).fingerprint !== data.fingerprint) throw notFound();
          reply(response, nodeLink.approveEnrollment(current.accountId, data.humanCode, current.identity.verifiedMailboxes[0] ?? current.identity.subject)); return;
        }
        if (route === 'GET /api/session') { reply(response, { accountId: actor.accountId, csrf: actor.csrf, expiresAt: actor.expiresAt, name: actor.identity.subject }); return; }
        if (route === 'GET /api/nodes') { reply(response, browser.nodes(actor.accountId)); return; }
        if (route === 'GET /api/projects') {
          const projects = authority.all("SELECT p.id,h.host,p.name FROM projects p JOIN host_bindings h ON h.project_id=p.id JOIN memberships m ON m.project_id=p.id WHERE p.status='active' AND h.active=1 AND m.account_id=? AND m.status='active' LIMIT 100", actor.accountId);
          reply(response, projects.map(project => ({ ...project, role: authority.member(gateway.project(String(project.host), actor.accountId, String(project.id))).role }))); return;
        }
        if (route !== 'POST /api/projects') {
          const data = await body(request);
          sessions.get(request, host, true);
          limit(`manage:${actor.accountId}`, 60);
          if (route === 'POST /api/agents') { reply(response, browser.registerAgent(actor.accountId, data)); return; }
          const input = closedObject(data, route === 'POST /api/project-options' ? ['projectHost'] : ['projectHost', 'agentId', 'policy', 'providerMaxCostMicros']);
          if (typeof input.projectHost !== 'string') throw notFound();
          const ctx = gateway.project(input.projectHost, actor.accountId); authority.member(ctx);
          if (route === 'POST /api/project-options') {
            reply(response, { projectId: ctx.projectId, members: browser.members(ctx), sections: authority.all('SELECT id,name FROM sections WHERE project_id=? AND archived=0 LIMIT 100', ctx.projectId) }); return;
          }
          reply(response, browser.propose(ctx, { agentId: input.agentId, policy: input.policy, providerMaxCostMicros: input.providerMaxCostMicros })); sweep(); return;
        }
        limit(`create:${actor.accountId}`, 10);
        const data = closedObject(await body(request), ['slug', 'name']);
        sessions.get(request, host, true);
        if (data.slug === 'hub-origin' || typeof data.slug === 'string' && data.slug.startsWith('xn--')) throw notFound();
        reply(response, authority.createProject(actor.accountId, data.slug as string, data.name as string)); return;
      }
      const ctx = gateway.project(host, actor.accountId, actor.projectId);
      if (route === 'GET /api/invitation-session') { reply(response, { accountId: actor.accountId, name: actor.identity.subject, csrf: actor.csrf, expiresAt: actor.expiresAt }); return; }
      if (route === 'POST /api/invitations/accept') {
        const data = await body(request); const current = sessions.get(request, host, true);
        reply(response, browser.accept(ctx, data, current.identity)); sweep(); return;
      }
      authority.member(ctx);
      if (route === 'GET /api/session') { reply(response, { accountId: actor.accountId, projectId: ctx.projectId, role: authority.member(ctx).role, csrf: actor.csrf, expiresAt: actor.expiresAt, name: actor.identity.subject }); return; }
      if (route === 'GET /api/members') { reply(response, browser.members(ctx)); return; }
      if (route === 'GET /api/invitations') { reply(response, browser.invitations(ctx)); return; }
      if (route === 'GET /api/participants') { reply(response, browser.participants(ctx)); return; }
      if (route === 'GET /api/turns') { reply(response, browser.turns(ctx, id(url.searchParams.get('sectionId')))); return; }
      if (route === 'GET /api/project') { reply(response, authority.one('SELECT id,slug,name,revision FROM projects WHERE id=?', ctx.projectId)); return; }
      if (route === 'GET /api/sections') { reply(response, authority.all('SELECT id,name,revision FROM sections WHERE project_id=? AND archived=0 LIMIT 100', ctx.projectId)); return; }
      if (route === 'GET /api/history') { reply(response, routing.history(ctx, id(url.searchParams.get('sectionId')))); return; }
      if (route === 'GET /api/turn') { reply(response, routing.turn(ctx, id(url.searchParams.get('turnId')))); return; }
      if (route === 'GET /api/events') { subscribe(request, response, host, url.searchParams.has('sectionId') ? id(url.searchParams.get('sectionId')) : undefined); return; }
      const data = await body(request);
      // Body streaming yielded; all authority and session evidence is checked again before mutation.
      sessions.get(request, host, true); authority.member(ctx);
      if (route === 'POST /api/members') { const input = closedObject(data, ['accountId', 'revision', 'role']); authority.editMember(ctx, id(input.accountId), input.revision as number, input.role as 'member'); reply(response, { updated: true }); }
      else if (route === 'POST /api/invitations') reply(response, browser.invite(ctx, data));
      else if (route === 'POST /api/invitations/revoke') { const input = closedObject(data, ['invitationId']); authority.revokeInvite(ctx, id(input.invitationId)); reply(response, { revoked: true }); }
      else if (route === 'POST /api/participants/consent') { const input = closedObject(data, ['participationId', 'revision', 'policyDigest']); authority.consent(ctx, id(input.participationId), input.revision as number, input.policyDigest as string, 'project'); reply(response, { approved: true }); }
      else if (route === 'POST /api/participants/withdraw') { const input = closedObject(data, ['participationId']); authority.withdraw(ctx, id(input.participationId)); reply(response, { withdrawn: true }); }
      else if (route === 'POST /api/sections') { const input = closedObject(data, ['name']); reply(response, { sectionId: authority.createSection(ctx, input.name as string) }); }
      else { limit(`post:${actor.accountId}`, 60); reply(response, routing.post(ctx, data as PostMessage)); }
      sweep();
    } catch (error) {
      if (response.headersSent) { response.end(); return; }
      const failure = error instanceof ProtocolError ? error : new ProtocolError(500, 'internal_error', 'Request failed.');
      response.statusCode = failure.status;
      if (failure.status === 413) { response.setHeader('Connection', 'close'); request.resume(); }
      if (failure.status === 429) response.setHeader('Retry-After', '60');
      reply(response, { error: { code: failure.code, message: failure.message } });
    }
  });
  function sweep() { for (const stream of streams) stream.tick(); }
  const interval = setInterval(() => {
    sweep();
    for (const [key, row] of rate) if (row.until <= authority.now()) rate.delete(key);
    try { sessions.prune(); } catch { /* Recovery fences requests/streams; maintenance resumes after activation. */ }
  }, options.pollIntervalMs ?? 250);
  interval.unref();
  server.on('upgrade', (request, socket, head) => {
    try {
      const host = gateway.resolve(request);
      if (host !== gateway.accountsHost || request.method !== 'GET' || request.url !== '/node' || singleHeader(request, 'origin') !== undefined || singleHeader(request, 'cookie') !== undefined || !upgradeHandler) throw notFound();
      if (upgrades.size >= 100) throw notFound();
      upgrades.add(socket); socket.on('close', () => upgrades.delete(socket));
      Promise.resolve(upgradeHandler(request, socket, head, Object.freeze({ host, authority, routing }))).catch(() => socket.destroy());
    } catch { socket.end('HTTP/1.1 404 Not Found\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: 0\r\n\r\n'); }
  });
  server.on('close', () => clearInterval(interval));
  function mountNodeUpgrade(handler: NodeUpgradeHandler) {
    if (upgradeHandler) throw new Error('Node upgrade handler already mounted.');
    upgradeHandler = handler;
    return () => { if (upgradeHandler === handler) upgradeHandler = undefined; };
  }
  if (options.nodeLink) {
    nodeLink = attachNodeLink({ mountNodeUpgrade }, { ...options.nodeLink, authority, routing });
  }
  return {
    server, authority, routing, gateway, sessions, nodeLink, mountNodeUpgrade,
    async close() {
      await nodeLink?.close();
      clearInterval(interval); for (const stream of [...streams]) stream.close(); for (const socket of upgrades) socket.destroy();
      await new Promise<void>((resolve, reject) => { if (!server.listening) { resolve(); return; } server.close(error => error ? reject(error) : resolve()); server.closeIdleConnections(); });
    },
  };
}

/** Operator-owned config points only at a new, isolated hub database. No implicit local data/config. */
export function startHub(config: {
  domain: string; accountsHost: string; dataPath: string; mailboxKeyPath: string; port?: number; trustedGatewayAddresses?: string[];
  web?: { distPath: string; developmentIdentity?: boolean };
  nodeLink?: { signingKeyPath: string };
  identity: { mode: 'clerk'; issuer: string; jwksPath: string; audience?: string } | { mode: 'stub'; tokensPath: string };
}) {
  if (!Number.isInteger(config.port ?? 4318) || (config.port ?? 4318) < 0 || (config.port ?? 4318) > 65535 || !config.dataPath || !config.mailboxKeyPath) throw new Error('Explicit hub data/key paths and valid port required.');
  const mailboxKey = readFileSync(config.mailboxKeyPath);
  const identity = config.identity;
  let verifier: SessionVerifier;
  if (identity.mode === 'clerk') verifier = createClerkVerifier({ issuer: identity.issuer, accountsOrigin: `https://${config.accountsHost}`, audience: identity.audience, jwks: JSON.parse(readFileSync(identity.jwksPath, 'utf8')) });
  else if (identity.mode === 'stub') verifier = createStubVerifier(new Map(Object.entries(JSON.parse(readFileSync(identity.tokensPath, 'utf8'))) as [string, SessionIdentity][]));
  else throw new Error('Explicit clerk or stub identity mode required.');
  const storage = openHubDatabase(config.dataPath);
  try {
    const hub = createHubServer({ authority: createHubAuthority(storage.db, { domain: config.domain, mailboxKey }), verifier, accountsHost: config.accountsHost, domain: config.domain, trustedGatewayAddresses: config.trustedGatewayAddresses, web: config.web, ...(config.nodeLink ? { nodeLink: { origin: `https://${config.accountsHost}`, signingKey: createPrivateKey(readFileSync(config.nodeLink.signingKeyPath)) } } : {}) });
    hub.server.once('close', () => storage.close()); hub.server.once('error', () => { void hub.close().finally(() => storage.close()); });
    hub.server.listen(config.port ?? 4318, '127.0.0.1');
    return hub;
  } catch (error) { storage.close(); throw error; }
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const file = process.env.MINARBOT_HUB_CONFIG;
  if (!file) throw new Error('MINARBOT_HUB_CONFIG must name an explicit isolated hub JSON configuration.');
  const hub = startHub(JSON.parse(readFileSync(file, 'utf8')));
  for (const signal of ['SIGINT', 'SIGTERM'] as const) process.once(signal, () => { void hub.close(); });
}

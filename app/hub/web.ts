import { randomBytes } from 'node:crypto';
import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { closedObject, id, ProtocolError, sha256 } from '../shared/protocol.ts';
import { parsePolicy, type HubAuthority, type VerifiedIdentity, type ProjectContext } from './project-authority.ts';

/** Only assets reachable from the hub entry are served. Local-owner bundles stay outside this allowlist. */
export function hubAssets(distPath: string) {
  type Entry = { file: string; imports?: string[]; dynamicImports?: string[]; css?: string[]; assets?: string[] };
  const manifest = JSON.parse(readFileSync(resolve(distPath, '.vite/manifest.json'), 'utf8')) as Record<string, Entry>;
  const files = new Map<string, Buffer>(), seen = new Set<string>();
  function add(file: string) {
    if (!/^assets\/[A-Za-z0-9_.-]+$/.test(file)) throw new Error('Invalid hub build asset.');
    files.set(`/${file}`, readFileSync(resolve(distPath, file)));
  }
  function visit(key: string) {
    if (seen.has(key)) return; seen.add(key);
    const entry = manifest[key]; if (!entry) throw new Error('Hub entry missing from build manifest.');
    add(entry.file); for (const file of [...entry.css ?? [], ...entry.assets ?? []]) add(file);
    for (const child of [...entry.imports ?? [], ...entry.dynamicImports ?? []]) visit(child);
  }
  visit('hub.html');
  const html = readFileSync(resolve(distPath, 'hub.html'), 'utf8');
  return { files, html };
}
export function hubLeptosAssets(distPath: string) {
  const leptos = resolve(distPath, '../_leptos');
  const manifest = resolve(leptos, 'hub-assets.json');
  if (!existsSync(manifest)) return undefined;
  const paths: unknown = JSON.parse(readFileSync(manifest, 'utf8'));
  if (!Array.isArray(paths)) throw new Error('Invalid Leptos hub manifest.');
  const files = new Map<string, Buffer>(), gzip = new Map<string, Buffer>();
  for (const path of paths) {
    if (typeof path !== 'string' || !/^\/_leptos\/assets\/[a-zA-Z0-9_-]+\.[a-f0-9]{16}\.(?:js|css|svg|woff2|wasm)$/.test(path)) throw new Error('Invalid Leptos hub asset.');
    const file = resolve(leptos, path.slice('/_leptos/'.length));
    files.set(path, readFileSync(file));
    if (existsSync(file + '.gz')) gzip.set(path, readFileSync(file + '.gz'));
  }
  const html = readFileSync(resolve(leptos, 'hub.html'), 'utf8');
  const startup = /<script type="module" src="([^"]+)"/.exec(html)?.[1];
  if (!startup || !files.has(startup)) throw new Error('Leptos hub startup module is missing from the asset manifest.');
  return { files, gzip, html };
}
export function hubDocument(html: string, config: { accountsHost: string; domain: string; developmentIdentity: boolean }) {
  const nonce = randomBytes(24).toString('base64url');
  const safe = JSON.stringify(config).replaceAll('<', '\\u003c').replaceAll('>', '\\u003e').replaceAll('&', '\\u0026');
  return { nonce, html: html.replace('<head>', `<head><meta name="csp-nonce" content="${nonce}"><script nonce="${nonce}" type="application/json" id="hub-config">${safe}</script>`) };
}

/** Browser-only authority adapters. Account and project context always originate in the current session. */
export function createHubWebApi(authority: HubAuthority) {
  const { all, one } = authority;
  authority.db.exec(`CREATE TABLE IF NOT EXISTS hub_web_requests (
    participation_id TEXT PRIMARY KEY, revision INTEGER NOT NULL, requester_account_id TEXT NOT NULL,
    provider_max_cost_micros INTEGER NOT NULL) STRICT;`);
  function members(ctx: ProjectContext) {
    authority.member(ctx);
    return all("SELECT m.account_id AS accountId,a.subject AS name,m.role,m.status,m.revision FROM memberships m JOIN accounts a ON a.id=m.account_id WHERE m.project_id=? AND m.status='active' LIMIT 100", ctx.projectId);
  }
  return {
    members,
    invitations(ctx: ProjectContext) {
      authority.member(ctx, 'invite');
      return all('SELECT id,role,state,expires_at AS expiresAt,accepted_by AS acceptedBy FROM invitations WHERE project_id=? ORDER BY rowid DESC LIMIT 100', ctx.projectId).map(row => ({ ...row, state: row.state === 'pending' && Number(row.expiresAt) <= authority.now() ? 'expired' : row.state }));
    },
    participants(ctx: ProjectContext) {
      authority.member(ctx);
      return all('SELECT p.id,p.agent_id AS agentId,a.name,a.owner_account_id AS ownerAccountId,ac.subject AS ownerName,n.id AS nodeId,n.status AS nodeStatus,p.state,p.revision,p.owner_consent_revision AS ownerConsentRevision,p.project_consent_revision AS projectConsentRevision,p.policy_json AS policy FROM participations p JOIN agents a ON a.id=p.agent_id JOIN accounts ac ON ac.id=a.owner_account_id JOIN nodes n ON n.id=a.home_node_id WHERE p.project_id=? LIMIT 100', ctx.projectId).map((row): Record<string, unknown> => ({ ...row, policy: JSON.parse(String(row.policy)), policyDigest: sha256(String(row.policy)) }));
    },
    turns(ctx: ProjectContext, sectionId: string) {
      authority.section(ctx, sectionId);
      return all('SELECT t.id,t.message_id AS messageId,p.agent_id AS agentId,t.requester_account_id AS requesterAccountId,t.payer_account_id AS payerAccountId,t.state FROM turns t JOIN participations p ON p.project_id=t.project_id AND p.id=t.participation_id WHERE t.project_id=? AND t.section_id=? ORDER BY t.rowid DESC LIMIT 100', ctx.projectId, sectionId);
    },
    nodes(accountId: string) {
      authority.account(accountId);
      return all('SELECT id,status,installation_id AS installationId FROM nodes WHERE owner_account_id=? LIMIT 100', accountId);
    },
    registerAgent(accountId: string, input: unknown) {
      const data = closedObject(input, ['agentId', 'nodeId', 'name']);
      const agentId = id(data.agentId), nodeId = id(data.nodeId);
      const prior = one('SELECT owner_account_id,home_node_id,name FROM agents WHERE id=?', agentId);
      if (prior) {
        authority.account(accountId);
        if (prior.owner_account_id !== accountId || prior.home_node_id !== nodeId || prior.name !== data.name) throw new ProtocolError(403, 'authority_denied', 'Agent identity does not match this account and node.');
        return { agentId };
      }
      return { agentId: authority.registerAgent(accountId, nodeId, agentId, data.name as string) };
    },
    propose(ctx: ProjectContext, input: unknown) {
      const data = closedObject(input, ['agentId', 'policy', 'providerMaxCostMicros']);
      const policy = parsePolicy(data.policy), cost = data.providerMaxCostMicros ?? policy.maxCostMicros;
      if (!Number.isSafeInteger(cost) || Number(cost) < 1 || Number(cost) > policy.maxCostMicros) throw new ProtocolError(400, 'invalid_policy', 'Provider request bound must fit the turn ceiling.');
      const proposal = authority.propose(ctx, id(data.agentId), policy);
      authority.db.prepare('INSERT OR REPLACE INTO hub_web_requests VALUES(?,?,?,?)').run(proposal.participationId, proposal.revision, ctx.accountId, Number(cost));
      return proposal;
    },
    invite(ctx: ProjectContext, input: unknown) {
      const data = closedObject(input, ['mailbox', 'role']);
      const invitation = authority.invite(ctx, { mailbox: data.mailbox as string, role: data.role as 'member', expiresAt: authority.now() + 86400000 });
      // ponytail: pilot uses private invite links; add mail delivery when email transport is configured.
      // Proof is never persisted as plaintext or placed in query strings.
      return { invitationId: invitation.invitationId, expiresAt: invitation.expiresAt, url: `https://${ctx.host}/invite#${new URLSearchParams({ invitationId: invitation.invitationId, token: invitation.token })}` };
    },
    accept(ctx: ProjectContext, input: unknown, identity: VerifiedIdentity) {
      const data = closedObject(input, ['invitationId', 'token']);
      return authority.acceptInvite(ctx, id(data.invitationId), data.token as string, identity);
    },
  };
}

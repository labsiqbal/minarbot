import { createHmac, randomBytes, randomUUID } from 'node:crypto';
import type { DatabaseSync, SQLInputValue } from 'node:sqlite';
import { canonicalJson, closedObject, digest, existingRevision, id, normalizeEmail, ProtocolError, publicKey, sha256, time } from '../shared/protocol.ts';
import { transaction } from './schema.ts';

export type Role = 'owner' | 'admin' | 'member' | 'viewer';
export const PERMISSIONS = {
  read: ['owner', 'admin', 'member', 'viewer'], post: ['owner', 'admin', 'member'],
  propose: ['owner', 'admin', 'member'], withdraw: ['owner', 'admin', 'member', 'viewer'],
  manageSections: ['owner', 'admin'], approveParticipation: ['owner', 'admin'],
  invite: ['owner', 'admin'], moderate: ['owner', 'admin'], manageAdmins: ['owner'], transferOwner: ['owner'], deleteProject: ['owner'],
} satisfies Record<string, Role[]>;
export type Permission = keyof typeof PERMISSIONS;
export type ProjectContext = Readonly<{ projectId: string; host: string; accountId: string }>;
export type VerifiedIdentity = { issuer: string; subject: string; verifiedMailboxes: string[] };
export interface IdentityVerifier { verify(token: string): Promise<VerifiedIdentity> }
export type InvocationPolicy =
  | { mode: 'owner' }
  | { mode: 'members'; accountIds: string[] }
  | { mode: 'all-members' };
export type ParticipationPolicy = {
  sectionIds: string[]; invocation: InvocationPolicy; scopes: ('read' | 'invoke')[];
  localPolicyDigest: string; payerBindingRef: string; maxCostMicros: number;
};
export type NodePrincipal = Readonly<{ nodeId: string; accountId: string; nodeGeneration: number; connectionGeneration: number }>;
export type Fence = {
  authorityGeneration: string; requesterRevision: number; membershipRevision: number;
  ownerAccountRevision: number; ownerMembershipRevision: number; participationRevision: number;
  ownerConsentRevision: number; projectConsentRevision: number; nodeGeneration: number;
  connectionGeneration: number; keyRevision: number; agentRevision: number;
};
export type ControlFrame = { v: 1; kind: 'grant.revoked' | 'turn.cancel'; eventId: string; projectId: string; sectionId: string; participationId: string; nodeId: string; turnId?: string; authorityGeneration: string; revision: number };
type Row = Record<string, string | number | null>;
const missing = () => new ProtocolError(404, 'resource_not_found', 'Resource not found.');
export const denied = () => new ProtocolError(403, 'authority_denied', 'Current authority does not allow this action.');
export const stale = () => new ProtocolError(409, 'stale_revision', 'Authority changed. Refresh before retrying.');
const reserved = new Set(['accounts', 'account', 'api', 'assets', 'www', 'admin', 'status', 'mail', 'support', 'auth', 'login', 'static', 'cdn', 'hub', 'nodes', 'localhost']);
export function slug(value: unknown) {
  if (typeof value !== 'string' || !/^[a-z][a-z0-9-]{1,61}[a-z0-9]$/.test(value) || reserved.has(value)) throw new ProtocolError(400, 'invalid_slug', 'Choose an available lowercase project address (3-63 characters).');
  return value;
}
function text(value: unknown, max = 160) {
  if (typeof value !== 'string' || !value.trim() || value.length > max || /[\u0000-\u001f]/.test(value)) throw new ProtocolError(400, 'invalid_text', 'Invalid text.');
  return value;
}
function role(value: unknown): Role {
  if (typeof value !== 'string' || !['owner', 'admin', 'member', 'viewer'].includes(value)) throw new ProtocolError(400, 'invalid_role', 'Invalid role.');
  return value as Role;
}
function strings(value: unknown, parse: (v: unknown) => string, max = 100): string[] {
  if (!Array.isArray(value) || value.length > max) throw new ProtocolError(400, 'invalid_list', 'Invalid list.');
  const values = value.map(parse).sort();
  if (new Set(values).size !== values.length) throw new ProtocolError(400, 'invalid_list', 'Duplicate list entry.');
  return values;
}
export function parsePolicy(value: unknown): ParticipationPolicy {
  const p = closedObject(value, ['sectionIds', 'invocation', 'scopes', 'localPolicyDigest', 'payerBindingRef', 'maxCostMicros']);
  const sectionIds = strings(p.sectionIds, v => id(v));
  if (!sectionIds.length) throw new ProtocolError(400, 'invalid_policy', 'At least one section is required.');
  const inv = closedObject(p.invocation ?? { mode: 'owner' }, ['mode', 'accountIds']);
  let invocation: InvocationPolicy;
  if (inv.mode === 'members') invocation = { mode: 'members', accountIds: strings(inv.accountIds, v => id(v)) };
  else if ((inv.mode === 'owner' || inv.mode === 'all-members') && inv.accountIds === undefined) invocation = { mode: inv.mode };
  else throw new ProtocolError(400, 'invalid_policy', 'Invalid invocation policy.');
  const scopes = strings(p.scopes, v => { if (v !== 'read' && v !== 'invoke') throw denied(); return v; }) as ParticipationPolicy['scopes'];
  if (!scopes.includes('read') || !Number.isSafeInteger(p.maxCostMicros) || Number(p.maxCostMicros) < 0) throw new ProtocolError(400, 'invalid_policy', 'Read scope and a nonnegative local spend ceiling are required.');
  if (typeof p.payerBindingRef !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(p.payerBindingRef)) throw new ProtocolError(400, 'invalid_policy', 'Use an opaque node-local payer reference.');
  return { sectionIds, invocation, scopes, localPolicyDigest: digest(p.localPolicyDigest), payerBindingRef: p.payerBindingRef, maxCostMicros: Number(p.maxCostMicros) };
}

export function createHubAuthority(db: DatabaseSync, options: { domain: string; mailboxKey: Uint8Array; now?: () => number }) {
  if (!/^(?:[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.)+[a-z]{2,}$/.test(options.domain) || options.mailboxKey.length < 32) throw new Error('Exact lowercase hub domain and private mailbox lookup key required.');
  const now = options.now ?? Date.now;
  const one = (sql: string, ...args: SQLInputValue[]) => db.prepare(sql).get(...args) as Row | undefined;
  const all = (sql: string, ...args: SQLInputValue[]) => db.prepare(sql).all(...args) as Row[];
  const run = (sql: string, ...args: SQLInputValue[]) => db.prepare(sql).run(...args);
  function generation() {
    const state = one("SELECT * FROM hub_state WHERE id=1 AND state='active'");
    if (!state) throw new ProtocolError(503, 'authority_unavailable', 'Hub is awaiting recovery reconciliation.', true);
    return String(state.authority_generation);
  }
  function account(accountId: string) {
    generation(); const row = one("SELECT * FROM accounts WHERE id=? AND status='active' AND pilot_admitted=1", id(accountId));
    if (!row) throw new ProtocolError(401, 'account_unavailable', 'Account is not admitted or active.');
    return row;
  }
  function resolveProject(host: string, accountId: string, expectedProjectId?: string): ProjectContext {
    account(accountId);
    const p = one("SELECT p.id FROM host_bindings h JOIN projects p ON p.id=h.project_id WHERE h.host=? AND h.active=1 AND p.status='active'", host);
    if (!p || expectedProjectId !== undefined && id(expectedProjectId) !== p.id) throw missing();
    return Object.freeze({ projectId: String(p.id), host, accountId: id(accountId) });
  }
  function member(ctx: ProjectContext, permission: Permission = 'read') {
    const bound = resolveProject(ctx.host, ctx.accountId, ctx.projectId);
    const row = one("SELECT * FROM memberships WHERE project_id=? AND account_id=? AND status='active'", bound.projectId, bound.accountId);
    if (!row) throw missing();
    if (!(PERMISSIONS[permission] as readonly string[]).includes(String(row.role))) throw denied();
    return row;
  }
  function section(ctx: ProjectContext, sectionId: string, permission: Permission = 'read') {
    member(ctx, permission);
    const row = one('SELECT * FROM sections WHERE project_id=? AND id=? AND archived=0', ctx.projectId, id(sectionId));
    if (!row) throw missing(); return row;
  }
  function membership(projectId: string, accountId: string) {
    const row = one("SELECT * FROM memberships WHERE project_id=? AND account_id=? AND status='active' AND role!='viewer'", projectId, accountId);
    if (!row) throw denied(); account(accountId); return row;
  }
  function change(projectId: string, kind: string, subjectId: string, revision: number) {
    run('INSERT INTO authority_events(project_id,kind,subject_id,revision,created_at) VALUES(?,?,?,?,?)', projectId, kind, subjectId, revision, now());
  }
  function cancelTurns(projectId: string, where: string, value: string) {
    const turns = all(`SELECT * FROM turns WHERE project_id=? AND ${where}=? AND state IN('pending','admitted')`, projectId, value);
    for (const turn of turns) {
      run("UPDATE turns SET state=CASE WHEN state='admitted' THEN 'unknown' ELSE 'revoked' END WHERE id=?", turn.id);
      run('DELETE FROM outbox WHERE turn_id=?', turn.id);
      control('turn.cancel', projectId, String(turn.section_id), String(turn.participation_id), String(turn.node_id), 0, String(turn.id));
    }
  }
  function control(kind: ControlFrame['kind'], projectId: string, sectionId: string, participationId: string, nodeId: string, revision: number, turnId?: string) {
    const frame: ControlFrame = { v: 1, kind, eventId: randomUUID(), projectId, sectionId, participationId, nodeId, authorityGeneration: generation(), revision, ...(turnId ? { turnId } : {}) };
    run('INSERT INTO outbox(event_id,project_id,section_id,participation_id,node_id,turn_id,kind,fence_json,frame_json,expires_at) VALUES(?,?,?,?,?,?,?,?,?,?)', frame.eventId, projectId, sectionId, participationId, nodeId, turnId ?? null, kind, '{}', canonicalJson(frame), now() + 600_000);
  }
  function invalidateParticipation(projectId: string, participationId: string) {
    const p = one('SELECT p.*,a.home_node_id FROM participations p JOIN agents a ON a.id=p.agent_id WHERE p.project_id=? AND p.id=?', projectId, participationId);
    if (!p) throw missing();
    run("UPDATE participations SET state='revoked',revision=revision+1,owner_consent_revision=NULL,project_consent_revision=NULL WHERE project_id=? AND id=?", projectId, participationId);
    run('DELETE FROM outbox WHERE project_id=? AND participation_id=?', projectId, participationId);
    cancelTurns(projectId, 'participation_id', participationId);
    for (const s of all('SELECT section_id FROM participation_sections WHERE project_id=? AND participation_id=?', projectId, participationId)) control('grant.revoked', projectId, String(s.section_id), participationId, String(p.home_node_id), Number(p.revision) + 1);
    change(projectId, 'participation.changed', participationId, Number(p.revision) + 1);
  }
  function node(principal: NodePrincipal, requireReady = true) {
    account(principal.accountId);
    const row = one("SELECT * FROM nodes WHERE id=? AND owner_account_id=? AND status!='revoked'", id(principal.nodeId), principal.accountId);
    if (!row) throw missing();
    if (row.generation !== existingRevision(principal.nodeGeneration) || row.connection_generation !== existingRevision(principal.connectionGeneration)) throw stale();
    if (requireReady && row.status !== 'ready') throw new ProtocolError(503, 'node_offline', 'Waiting for member node.', true);
    return row;
  }
  function participant(projectId: string, sectionId: string, participationId: string) {
    const p = one("SELECT p.*,a.owner_account_id,a.home_node_id,a.revision AS agent_revision FROM participations p JOIN agents a ON a.id=p.agent_id JOIN participation_sections s ON s.project_id=p.project_id AND s.participation_id=p.id JOIN sections sec ON sec.project_id=s.project_id AND sec.id=s.section_id JOIN projects pr ON pr.id=p.project_id WHERE p.project_id=? AND p.id=? AND s.section_id=? AND sec.archived=0 AND pr.status='active'", id(projectId), id(participationId), id(sectionId));
    if (!p) throw missing();
    const owner = account(String(p.owner_account_id)), m = membership(projectId, String(p.owner_account_id));
    if (p.state !== 'active' || p.owner_consent_revision !== p.revision || p.project_consent_revision !== p.revision || p.owner_membership_revision !== m.revision) throw denied();
    const n = one("SELECT * FROM nodes WHERE id=? AND owner_account_id=? AND status!='revoked'", p.home_node_id, p.owner_account_id);
    if (!n) throw denied();
    return { participation: p, owner, ownerMembership: m, node: n, policy: parsePolicy(JSON.parse(String(p.policy_json))) };
  }
  function authorize(ctx: ProjectContext, sectionId: string, participationId: string, invoke = false) {
    section(ctx, sectionId, invoke ? 'post' : 'read');
    const result = participant(ctx.projectId, sectionId, participationId), { participation: p, policy } = result;
    if (!policy.scopes.includes(invoke ? 'invoke' : 'read')) throw denied();
    if (invoke && ctx.accountId !== p.owner_account_id && policy.invocation.mode !== 'all-members' && !(policy.invocation.mode === 'members' && policy.invocation.accountIds.includes(ctx.accountId))) throw denied();
    return result;
  }
  function fence(ctx: ProjectContext, sectionId: string, participationId: string, invoke = true): Fence {
    const { participation: p, owner, ownerMembership: om, node: n } = authorize(ctx, sectionId, participationId, invoke);
    const m = member(ctx, invoke ? 'post' : 'read'), requester = account(ctx.accountId);
    return { authorityGeneration: generation(), requesterRevision: Number(requester.revision), membershipRevision: Number(m.revision), ownerAccountRevision: Number(owner.revision), ownerMembershipRevision: Number(om.revision), participationRevision: Number(p.revision), ownerConsentRevision: Number(p.owner_consent_revision), projectConsentRevision: Number(p.project_consent_revision), nodeGeneration: Number(n.generation), connectionGeneration: Number(n.connection_generation), keyRevision: Number(n.key_revision), agentRevision: Number(p.agent_revision) };
  }
  const mailboxDigest = (mailbox: string) => createHmac('sha256', options.mailboxKey).update(normalizeEmail(mailbox)).digest('hex');
  return {
    db, now, one, all, run, generation, account, resolveProject, member, section, node, participant, authorize, fence, invalidateParticipation,
    provisionAccount(identity: VerifiedIdentity, pilotAdmitted: boolean) {
      generation();
      const issuer = text(identity.issuer, 1024), subject = text(identity.subject, 256);
      if (typeof pilotAdmitted !== 'boolean') throw denied();
      const existing = one('SELECT * FROM accounts WHERE issuer=? AND subject=?', issuer, subject);
      if (existing) return String(existing.id);
      const accountId = randomUUID();
      run("INSERT INTO accounts(id,issuer,subject,status,pilot_admitted) VALUES(?,?,?,'active',?)", accountId, issuer, subject, Number(pilotAdmitted));
      return accountId;
    },
    async authenticate(verifier: IdentityVerifier, token: string) {
      const identity = await verifier.verify(token);
      const row = one('SELECT id FROM accounts WHERE issuer=? AND subject=?', text(identity.issuer, 1024), text(identity.subject, 256));
      if (!row) throw new ProtocolError(401, 'account_unavailable', 'Pilot invitation required.');
      account(String(row.id)); return { accountId: String(row.id), identity };
    },
    createProject(accountId: string, address: string, name: string) {
      return transaction(db, () => {
        account(accountId); slug(address); text(name);
        if (one('SELECT 1 FROM projects WHERE slug=?', address)) throw new ProtocolError(409, 'slug_unavailable', 'Project address is unavailable.');
        const projectId = randomUUID(), sectionId = randomUUID();
        run('INSERT INTO projects(id,owner_account_id,slug,name) VALUES(?,?,?,?)', projectId, accountId, address, name);
        run("INSERT INTO memberships(project_id,account_id,role) VALUES(?,?,'owner')", projectId, accountId);
        run('INSERT INTO host_bindings(host,project_id) VALUES(?,?)', `${address}.${options.domain}`, projectId);
        run("INSERT INTO sections(id,project_id,name) VALUES(?,?,'general')", sectionId, projectId);
        return { projectId, sectionId, host: `${address}.${options.domain}` };
      });
    },
    createSection(ctx: ProjectContext, name: string) {
      return transaction(db, () => { member(ctx, 'manageSections'); text(name); const sectionId = randomUUID(); run('INSERT INTO sections(id,project_id,name) VALUES(?,?,?)', sectionId, ctx.projectId, name); return sectionId; });
    },
    archiveSection(ctx: ProjectContext, sectionId: string, revision: number) {
      transaction(db, () => { const s = section(ctx, sectionId, 'manageSections'); if (s.revision !== existingRevision(revision)) throw stale();
        for (const p of all('SELECT participation_id FROM participation_sections WHERE project_id=? AND section_id=?', ctx.projectId, sectionId)) invalidateParticipation(ctx.projectId, String(p.participation_id));
        run('UPDATE sections SET archived=1,revision=revision+1 WHERE project_id=? AND id=?', ctx.projectId, sectionId); change(ctx.projectId, 'section.changed', sectionId, revision + 1);
      });
    },
    invite(ctx: ProjectContext, input: { mailbox: string; role: Exclude<Role, 'owner'>; expiresAt: number; targetAccountId?: string }) {
      return transaction(db, () => {
        const m = member(ctx, 'invite'); closedObject(input, ['mailbox', 'role', 'expiresAt', 'targetAccountId']);
        const r = role(input.role), expires = time(input.expiresAt);
        if (r === 'owner' || r === 'admin' && m.role !== 'owner') throw denied();
        if (expires <= now() || expires > now() + 86_400_000) throw new ProtocolError(400, 'invalid_expiry', 'Invitation must expire within 24 hours.');
        const target = input.targetAccountId ? id(input.targetAccountId) : null; if (target) account(target);
        const invitationId = randomUUID(), token = randomBytes(32).toString('base64url');
        run('INSERT INTO invitations(id,project_id,issuer_account_id,issuer_revision,mailbox_digest,target_account_id,token_digest,role,expires_at,authority_generation) VALUES(?,?,?,?,?,?,?,?,?,?)', invitationId, ctx.projectId, ctx.accountId, m.revision, mailboxDigest(input.mailbox), target, sha256(token), r, expires, generation());
        return { invitationId, token, expiresAt: expires };
      });
    },
    acceptInvite(ctx: ProjectContext, invitationId: string, token: string, identity: VerifiedIdentity) {
      return transaction(db, () => {
        resolveProject(ctx.host, ctx.accountId, ctx.projectId);
        const a = account(ctx.accountId);
        if (a.issuer !== identity.issuer || a.subject !== identity.subject) throw denied();
        const i = one('SELECT * FROM invitations WHERE project_id=? AND id=?', ctx.projectId, id(invitationId));
        if (!i || typeof token !== 'string' || token.length > 128 || i.token_digest !== sha256(token)) throw missing();
        if (!identity.verifiedMailboxes.some(mailbox => mailboxDigest(mailbox) === i.mailbox_digest) || i.target_account_id && i.target_account_id !== ctx.accountId) throw denied();
        if (i.state !== 'pending' || Number(i.expires_at) <= now() || i.authority_generation !== generation()) throw new ProtocolError(409, 'invitation_unavailable', 'Invitation expired, used, held or revoked.');
        const issuer = membership(ctx.projectId, String(i.issuer_account_id));
        if (issuer.revision !== i.issuer_revision || !['owner','admin'].includes(String(issuer.role)) || i.role === 'admin' && issuer.role !== 'owner') throw denied();
        const old = one('SELECT * FROM memberships WHERE project_id=? AND account_id=?', ctx.projectId, ctx.accountId);
        if (old?.status === 'active') throw new ProtocolError(409, 'already_member', 'Account is already a member.');
        run("INSERT INTO memberships(project_id,account_id,role) VALUES(?,?,?) ON CONFLICT(project_id,account_id) DO UPDATE SET role=excluded.role,status='active',revision=memberships.revision+1", ctx.projectId, ctx.accountId, i.role);
        run("UPDATE invitations SET state='accepted',accepted_by=?,revision=revision+1 WHERE id=?", ctx.accountId, invitationId);
        change(ctx.projectId, 'membership.changed', ctx.accountId, Number(old?.revision ?? 0) + 1);
        return { projectId: ctx.projectId, accountId: ctx.accountId, role: i.role as Role };
      });
    },
    revokeInvite(ctx: ProjectContext, invitationId: string) {
      transaction(db, () => { const m = member(ctx, 'invite'), i = one('SELECT * FROM invitations WHERE project_id=? AND id=?', ctx.projectId, id(invitationId));
        if (!i) throw missing(); if (i.role === 'admin' && m.role !== 'owner') throw denied();
        run("UPDATE invitations SET state='revoked',revision=revision+1 WHERE project_id=? AND id=? AND state IN('pending','held')", ctx.projectId, invitationId);
      });
    },
    editMember(ctx: ProjectContext, accountId: string, revision: number, newRole: Exclude<Role, 'owner'> | 'revoked') {
      transaction(db, () => {
        const m = member(ctx, 'invite'), target = one("SELECT * FROM memberships WHERE project_id=? AND account_id=? AND status='active'", ctx.projectId, id(accountId));
        if (!target) throw missing(); if (target.revision !== existingRevision(revision)) throw stale();
        if (newRole !== 'revoked') role(newRole);
        if (target.role === 'owner' || newRole === ('owner' as string) || m.role !== 'owner' && (target.role === 'admin' || newRole === 'admin')) throw denied();
        for (const p of all('SELECT p.id FROM participations p JOIN agents a ON a.id=p.agent_id WHERE p.project_id=? AND a.owner_account_id=?', ctx.projectId, accountId)) invalidateParticipation(ctx.projectId, String(p.id));
        cancelTurns(ctx.projectId, 'requester_account_id', accountId);
        run('UPDATE memberships SET role=?,status=?,revision=revision+1 WHERE project_id=? AND account_id=?', newRole === 'revoked' ? target.role : newRole, newRole === 'revoked' ? 'revoked' : 'active', ctx.projectId, accountId);
        change(ctx.projectId, 'membership.changed', accountId, revision + 1);
      });
    },
    transferOwner(ctx: ProjectContext, accountId: string) {
      transaction(db, () => { member(ctx, 'transferOwner'); id(accountId); membership(ctx.projectId, accountId);
        if (accountId === ctx.accountId) throw denied();
        run("UPDATE memberships SET role='admin',revision=revision+1 WHERE project_id=? AND account_id=?", ctx.projectId, ctx.accountId);
        run("UPDATE memberships SET role='owner',revision=revision+1 WHERE project_id=? AND account_id=?", ctx.projectId, accountId);
        run('UPDATE projects SET owner_account_id=?,revision=revision+1 WHERE id=?', accountId, ctx.projectId);
        for (const p of all('SELECT id FROM participations WHERE project_id=?', ctx.projectId)) invalidateParticipation(ctx.projectId, String(p.id));
        cancelTurns(ctx.projectId, 'requester_account_id', ctx.accountId); cancelTurns(ctx.projectId, 'requester_account_id', accountId);
        change(ctx.projectId, 'project.changed', ctx.projectId, Number(one('SELECT revision FROM projects WHERE id=?', ctx.projectId)!.revision));
      });
    },
    deleteProject(ctx: ProjectContext) {
      transaction(db, () => { member(ctx, 'deleteProject');
        for (const p of all('SELECT id FROM participations WHERE project_id=?', ctx.projectId)) invalidateParticipation(ctx.projectId, String(p.id));
        run("UPDATE projects SET status='deleted',revision=revision+1 WHERE id=?", ctx.projectId);
        run('UPDATE host_bindings SET active=0,generation=generation+1 WHERE project_id=?', ctx.projectId);
        run("UPDATE invitations SET state='revoked',revision=revision+1 WHERE project_id=? AND state IN('pending','held')", ctx.projectId);
        change(ctx.projectId, 'project.deleted', ctx.projectId, Number(one('SELECT revision FROM projects WHERE id=?', ctx.projectId)!.revision));
      });
    },
    registerNode(accountId: string, input: { id: string; installationId: string; signingPublicKey: string; encryptionPublicKey: string; capabilities: string[] }) {
      account(accountId); closedObject(input, ['id','installationId','signingPublicKey','encryptionPublicKey','capabilities']);
      const nodeId = id(input.id), installationId = id(input.installationId), signing = publicKey(input.signingPublicKey, 'ed25519'), encryption = publicKey(input.encryptionPublicKey, 'x25519');
      const capabilities = strings(input.capabilities, v => text(v));
      if (one('SELECT 1 FROM nodes WHERE id=? OR installation_id=?', nodeId, installationId)) throw new ProtocolError(409, 'node_exists', 'Installation is already enrolled.');
      run('INSERT INTO nodes(id,owner_account_id,installation_id,signing_public_key,encryption_public_key,capabilities_json) VALUES(?,?,?,?,?,?)', nodeId, accountId, installationId, signing.encoded, encryption.encoded, canonicalJson(capabilities));
      return nodeId;
    },
    connectNode(accountId: string, nodeId: string, expectedGeneration: number): NodePrincipal {
      return transaction(db, () => { account(accountId);
        const n = one("SELECT * FROM nodes WHERE id=? AND owner_account_id=? AND status!='revoked'", id(nodeId), accountId);
        if (!n) throw missing(); if (n.generation !== existingRevision(expectedGeneration)) throw stale();
        run("UPDATE nodes SET status='ready',connection_generation=connection_generation+1,last_seen=? WHERE id=?", now(), nodeId);
        run("UPDATE turns SET state='unknown' WHERE node_id=? AND state IN('pending','admitted')", nodeId);
        run("DELETE FROM outbox WHERE node_id=? AND kind NOT IN('grant.revoked','turn.cancel')", nodeId);
        return Object.freeze({ nodeId, accountId, nodeGeneration: Number(n.generation), connectionGeneration: Number(n.connection_generation) + 1 });
      });
    },
    revokeNode(accountId: string, nodeId: string) {
      transaction(db, () => { account(accountId); const n = one('SELECT * FROM nodes WHERE id=? AND owner_account_id=?', id(nodeId), accountId); if (!n) throw missing();
        for (const p of all('SELECT p.project_id,p.id FROM participations p JOIN agents a ON a.id=p.agent_id WHERE a.home_node_id=?', nodeId)) invalidateParticipation(String(p.project_id), String(p.id));
        run("UPDATE nodes SET status='revoked',generation=generation+1,connection_generation=connection_generation+1 WHERE id=?", nodeId);
      });
    },
    disconnectNode(principal: NodePrincipal) {
      transaction(db, () => { node(principal);
        run("UPDATE nodes SET status='offline' WHERE id=?", principal.nodeId);
        run("UPDATE turns SET state='unknown' WHERE node_id=? AND state IN('pending','admitted')", principal.nodeId);
        run("DELETE FROM outbox WHERE node_id=? AND kind NOT IN('grant.revoked','turn.cancel')", principal.nodeId);
      });
    },
    setAccountStatus(accountId: string, status: 'active' | 'blocked') {
      transaction(db, () => { generation(); id(accountId); if (status !== 'active' && status !== 'blocked') throw denied();
        const target = one('SELECT revision FROM accounts WHERE id=?', accountId); if (!target) throw missing();
        for (const p of all('SELECT p.project_id,p.id FROM participations p JOIN agents a ON a.id=p.agent_id WHERE a.owner_account_id=?', accountId)) invalidateParticipation(String(p.project_id), String(p.id));
        for (const m of all('SELECT project_id,revision FROM memberships WHERE account_id=?', accountId)) {
          cancelTurns(String(m.project_id), 'requester_account_id', accountId); change(String(m.project_id), 'account.changed', accountId, Number(target.revision) + 1);
        }
        run("UPDATE nodes SET generation=generation+1,connection_generation=connection_generation+1,status=CASE WHEN status='revoked' THEN status ELSE 'offline' END WHERE owner_account_id=?", accountId);
        run('UPDATE accounts SET status=?,revision=revision+1 WHERE id=?', status, accountId);
      });
    },
    registerAgent(accountId: string, nodeId: string, agentId: string, name: string) {
      account(accountId); id(nodeId); id(agentId); text(name);
      if (!one("SELECT 1 FROM nodes WHERE id=? AND owner_account_id=? AND status!='revoked'", nodeId, accountId)) throw missing();
      run('INSERT INTO agents(id,owner_account_id,home_node_id,name) VALUES(?,?,?,?)', agentId, accountId, nodeId, name); return agentId;
    },
    propose(ctx: ProjectContext, agentId: string, input: ParticipationPolicy) {
      return transaction(db, () => { const m = member(ctx, 'propose'), policy = parsePolicy(input);
        if (!one('SELECT 1 FROM agents WHERE id=? AND owner_account_id=?', id(agentId), ctx.accountId)) throw missing();
        for (const sectionId of policy.sectionIds) section(ctx, sectionId);
        if (policy.invocation.mode === 'members') for (const requester of policy.invocation.accountIds) membership(ctx.projectId, requester);
        const old = one('SELECT * FROM participations WHERE project_id=? AND agent_id=?', ctx.projectId, agentId);
        if (old) invalidateParticipation(ctx.projectId, String(old.id));
        const participationId = old ? String(old.id) : randomUUID();
        if (old) run("UPDATE participations SET policy_json=?,owner_membership_revision=?,state='proposed' WHERE id=?", canonicalJson(policy), m.revision, participationId);
        else run('INSERT INTO participations(id,project_id,agent_id,owner_membership_revision,policy_json) VALUES(?,?,?,?,?)', participationId, ctx.projectId, agentId, m.revision, canonicalJson(policy));
        run('DELETE FROM participation_sections WHERE project_id=? AND participation_id=?', ctx.projectId, participationId);
        for (const s of policy.sectionIds) run('INSERT INTO participation_sections VALUES(?,?,?)', ctx.projectId, participationId, s);
        return { participationId, revision: Number(one('SELECT revision FROM participations WHERE id=?', participationId)!.revision), policyDigest: sha256(canonicalJson(policy)) };
      });
    },
    consent(ctx: ProjectContext, participationId: string, revision: number, policyDigest: string, side: 'owner' | 'project', principal?: NodePrincipal) {
      transaction(db, () => { member(ctx, side === 'project' ? 'approveParticipation' : 'propose');
        const p = one('SELECT p.*,a.owner_account_id,a.home_node_id FROM participations p JOIN agents a ON a.id=p.agent_id WHERE p.project_id=? AND p.id=?', ctx.projectId, id(participationId));
        if (!p) throw missing(); if (p.revision !== existingRevision(revision) || sha256(String(p.policy_json)) !== digest(policyDigest) || p.state === 'revoked') throw stale();
        const ownerMembership = membership(ctx.projectId, String(p.owner_account_id));
        if (ownerMembership.revision !== p.owner_membership_revision) throw stale();
        if (side === 'owner') {
          if (ctx.accountId !== p.owner_account_id || !principal) throw denied();
          node(principal); if (principal.nodeId !== p.home_node_id || principal.accountId !== ctx.accountId) throw denied();
        } else if (side !== 'project') throw denied();
        const column = side === 'owner' ? 'owner_consent_revision' : 'project_consent_revision';
        run(`UPDATE participations SET ${column}=? WHERE project_id=? AND id=?`, revision, ctx.projectId, participationId);
        run("UPDATE participations SET state='active' WHERE project_id=? AND id=? AND owner_consent_revision=revision AND project_consent_revision=revision", ctx.projectId, participationId);
        change(ctx.projectId, 'participation.changed', participationId, revision);
      });
    },
    withdrawNode(principal: NodePrincipal, projectId: string, participationId: string, revision: number) {
      transaction(db, () => {
        node(principal);
        const p = one('SELECT p.*,a.owner_account_id,a.home_node_id FROM participations p JOIN agents a ON a.id=p.agent_id WHERE p.project_id=? AND p.id=?', id(projectId), id(participationId));
        if (!p || p.home_node_id !== principal.nodeId || p.owner_account_id !== principal.accountId) throw denied();
        // Exact old revision is idempotent after revocation/reproposal; never revoke new consent.
        if (Number(p.revision) < existingRevision(revision)) throw stale();
        if (p.revision === revision && p.state !== 'revoked') invalidateParticipation(projectId, participationId);
      });
    },
    withdraw(ctx: ProjectContext, participationId: string) {
      transaction(db, () => { const m = member(ctx, 'withdraw'), p = one('SELECT p.id,a.owner_account_id FROM participations p JOIN agents a ON a.id=p.agent_id WHERE p.project_id=? AND p.id=?', ctx.projectId, id(participationId));
        if (!p) throw missing(); if (p.owner_account_id !== ctx.accountId && !['owner','admin'].includes(String(m.role))) throw denied();
        invalidateParticipation(ctx.projectId, participationId);
      });
    },
    authorityEvents(ctx: ProjectContext, after = 0) {
      member(ctx); if (!Number.isSafeInteger(after) || after < 0) throw denied();
      return all('SELECT * FROM authority_events WHERE project_id=? AND cursor>? ORDER BY cursor LIMIT 100', ctx.projectId, after);
    },
  };
}
export type HubAuthority = ReturnType<typeof createHubAuthority>;

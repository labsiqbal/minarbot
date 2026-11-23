import { randomUUID } from 'node:crypto';
import { canonicalJson, closedObject, digest, existingRevision, id, ProtocolError, sha256, time } from '../shared/protocol.ts';
import { denied, stale, type ControlFrame, type Fence, type HubAuthority, type NodePrincipal, type ProjectContext } from './authority.ts';
import { transaction } from './schema.ts';

export type FrameKind = 'section.event' | 'turn.request' | 'turn.admitted' | 'turn.partial' | 'turn.final' | 'turn.failed' | 'turn.approval.request' | 'turn.effect.receipt';
export type RoutingFrame = {
  v: 1; kind: FrameKind; eventId: string; projectId: string; sectionId: string;
  participationId: string; agentId: string; nodeId: string; turnId?: string;
  requesterAccountId: string; payerAccountId: string; payerBindingRef: string;
  fence: Fence; sequence: number; expiresAt: number; payloadDigest: string;
  payload: Record<string, unknown>;
};
export type HubFrame = RoutingFrame | ControlFrame;
const resultKinds = new Set<FrameKind>(['turn.admitted','turn.partial','turn.final','turn.failed','turn.approval.request','turn.effect.receipt']);
function messageText(value: unknown, max = 16_000) {
  if (typeof value !== 'string' || !value.trim() || value.length > max || value.includes('\0')) throw new ProtocolError(400, 'invalid_text', 'Message is empty or too large.');
  return value;
}
export function createHubRouting(authority: HubAuthority) {
  const a = authority, { db, one, all, run } = a;
  function context(projectId: string, accountId: string) {
    const host = one('SELECT host FROM host_bindings WHERE project_id=? AND active=1', projectId);
    if (!host) throw new ProtocolError(404, 'resource_not_found', 'Resource not found.');
    return a.resolveProject(String(host.host), accountId, projectId);
  }
  function current(frame: RoutingFrame) {
    const ctx = context(frame.projectId, frame.requesterAccountId);
    const actual = a.fence(ctx, frame.sectionId, frame.participationId, frame.kind !== 'section.event');
    const p = a.participant(frame.projectId, frame.sectionId, frame.participationId);
    if (canonicalJson(actual) !== canonicalJson(frame.fence) || p.participation.agent_id !== frame.agentId || p.node.id !== frame.nodeId || p.participation.owner_account_id !== frame.payerAccountId || p.policy.payerBindingRef !== frame.payerBindingRef) throw stale();
    if (p.node.status !== 'ready') throw new ProtocolError(503, 'node_offline', 'Waiting for member node.', true);
    return p;
  }
  function enqueue(frame: RoutingFrame) {
    const encoded = canonicalJson(frame);
    if (Buffer.byteLength(encoded) > 65_536) throw new ProtocolError(413, 'payload_too_large', 'Frame exceeds the limit.');
    run('INSERT INTO outbox(event_id,project_id,section_id,participation_id,node_id,turn_id,kind,fence_json,frame_json,expires_at) VALUES(?,?,?,?,?,?,?,?,?,?)', frame.eventId, frame.projectId, frame.sectionId, frame.participationId, frame.nodeId, frame.turnId ?? null, frame.kind, canonicalJson(frame.fence), encoded, frame.expiresAt);
  }
  function frame(ctx: ProjectContext, sectionId: string, participationId: string, kind: 'section.event' | 'turn.request', payload: Record<string, unknown>, turnId?: string): RoutingFrame {
    const p = a.authorize(ctx, sectionId, participationId, kind === 'turn.request');
    return { v: 1, kind, eventId: randomUUID(), projectId: ctx.projectId, sectionId, participationId,
      agentId: String(p.participation.agent_id), nodeId: String(p.node.id), ...(turnId ? { turnId } : {}),
      requesterAccountId: ctx.accountId, payerAccountId: String(p.participation.owner_account_id), payerBindingRef: p.policy.payerBindingRef,
      fence: a.fence(ctx, sectionId, participationId, kind === 'turn.request'), sequence: 1, expiresAt: a.now() + 600_000, payloadDigest: sha256(canonicalJson(payload)), payload };
  }
  function checkOutbox(principal: NodePrincipal, row: NonNullable<ReturnType<typeof one>>) {
    a.node(principal);
    if (row.node_id !== principal.nodeId) throw denied();
    const f = JSON.parse(String(row.frame_json)) as HubFrame;
    if (('fence' in f && Number(row.expires_at) <= a.now()) || ('authorityGeneration' in f && f.authorityGeneration !== a.generation())) throw stale();
    if ('fence' in f) {
      current(f);
      if (f.turnId) {
        const turn = one('SELECT state FROM turns WHERE project_id=? AND id=?', f.projectId, f.turnId);
        if (!turn || !['pending','admitted'].includes(String(turn.state))) throw stale();
      }
    }
    return f;
  }
  return {
    post(ctx: ProjectContext, value: { requestId: string; sectionId: string; text: string; mentions: string[] }) {
      return transaction(db, () => {
        closedObject(value, ['requestId','sectionId','text','mentions']);
        const requestId = id(value.requestId), sectionId = id(value.sectionId), text = messageText(value.text);
        if (!Array.isArray(value.mentions) || value.mentions.length > 16) throw denied();
        const mentions = value.mentions.map(v => id(v)).sort(); if (new Set(mentions).size !== mentions.length) throw denied();
        a.section(ctx, sectionId, 'post');
        for (const p of mentions) a.authorize(ctx, sectionId, p, true);
        const commandDigest = sha256(canonicalJson({ sectionId, text, mentions }));
        const receipt = one('SELECT * FROM command_receipts WHERE project_id=? AND actor_account_id=? AND request_id=?', ctx.projectId, ctx.accountId, requestId);
        if (receipt) {
          if (receipt.digest !== commandDigest) throw new ProtocolError(409, 'idempotency_conflict', 'Request ID already has a different payload.');
          return JSON.parse(String(receipt.result_json)) as { messageId: string; turnIds: string[] };
        }
        for (const p of mentions) if (a.authorize(ctx, sectionId, p, true).node.status !== 'ready') throw new ProtocolError(503, 'node_offline', 'Waiting for member node.', true);
        const queued = Number(one("SELECT count(*) AS n FROM turns WHERE project_id=? AND state IN ('pending','admitted') AND json_extract(request_json,'$.expiresAt')>?", ctx.projectId, a.now())!.n);
        if (mentions.length && queued + mentions.length > 100) throw new ProtocolError(429, 'project_queue_full', 'Project queue is full. Retry later.', true);
        const messageId = randomUUID(), turnIds: string[] = [];
        run('INSERT INTO messages(id,project_id,section_id,requester_account_id,text,created_at) VALUES(?,?,?,?,?,?)', messageId, ctx.projectId, sectionId, ctx.accountId, text, a.now());
        for (const p of all('SELECT participation_id FROM participation_sections WHERE project_id=? AND section_id=?', ctx.projectId, sectionId)) {
          let event: RoutingFrame;
          try { event = frame(ctx, sectionId, String(p.participation_id), 'section.event', { messageId, text }); current(event); }
          catch (error) { if (!(error instanceof ProtocolError)) throw error; continue; }
          enqueue(event);
        }
        for (const participationId of mentions) {
          const turnId = randomUUID(); turnIds.push(turnId);
          const request = frame(ctx, sectionId, participationId, 'turn.request', { messageId, text, maxCostMicros: a.participant(ctx.projectId, sectionId, participationId).policy.maxCostMicros }, turnId);
          run("INSERT INTO turns(id,project_id,section_id,participation_id,message_id,requester_account_id,payer_account_id,payer_binding_ref,node_id,command_digest,fence_json,request_json,state) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,'pending')", turnId, ctx.projectId, sectionId, participationId, messageId, ctx.accountId, request.payerAccountId, request.payerBindingRef, request.nodeId, commandDigest, canonicalJson(request.fence), canonicalJson(request));
          enqueue(request);
        }
        const result = { messageId, turnIds };
        run('INSERT INTO command_receipts VALUES(?,?,?,?,?)', ctx.projectId, ctx.accountId, requestId, commandDigest, canonicalJson(result));
        return result;
      });
    },
    pending(principal: NodePrincipal): HubFrame[] {
      a.node(principal);
      const frames: HubFrame[] = [];
      for (const row of all("SELECT * FROM outbox WHERE node_id=? AND acked=0 ORDER BY CASE WHEN kind IN ('grant.revoked','turn.cancel') THEN 0 ELSE 1 END,rowid LIMIT 100", principal.nodeId)) {
        try { frames.push(checkOutbox(principal, row)); }
        catch (error) { if (!(error instanceof ProtocolError)) throw error; run('DELETE FROM outbox WHERE event_id=?', row.event_id); }
      }
      return frames;
    },
    acknowledge(principal: NodePrincipal, eventId: string, frameDigest: string) {
      transaction(db, () => {
        a.node(principal); const row = one('SELECT * FROM outbox WHERE node_id=? AND event_id=?', principal.nodeId, id(eventId));
        if (!row) throw new ProtocolError(404, 'resource_not_found', 'Resource not found.');
        checkOutbox(principal, row);
        if (sha256(String(row.frame_json)) !== digest(frameDigest)) throw new ProtocolError(409, 'idempotency_conflict', 'Frame digest differs.');
        run('UPDATE outbox SET acked=1 WHERE event_id=?', eventId);
      });
    },
    grants(principal: NodePrincipal) {
      a.node(principal);
      return all('SELECT p.project_id,p.id,s.section_id FROM participations p JOIN agents ag ON ag.id=p.agent_id JOIN participation_sections s ON s.project_id=p.project_id AND s.participation_id=p.id WHERE ag.home_node_id=?', principal.nodeId).flatMap(p => {
        try {
          const active = a.participant(String(p.project_id), String(p.section_id), String(p.id));
          return [{ v: 1 as const, kind: 'grant.snapshot' as const, projectId: String(p.project_id), sectionId: String(p.section_id), participationId: String(p.id), agentId: String(active.participation.agent_id), nodeId: principal.nodeId, agentOwnerAccountId: principal.accountId, authorityGeneration: a.generation(), nodeGeneration: principal.nodeGeneration, connectionGeneration: principal.connectionGeneration, revision: Number(active.participation.revision), ownerConsentRevision: Number(active.participation.owner_consent_revision), projectConsentRevision: Number(active.participation.project_consent_revision), ownerMembershipRevision: Number(active.ownerMembership.revision), policy: active.policy }];
        } catch (error) { if (!(error instanceof ProtocolError)) throw error; return []; }
      });
    },
    lease(principal: NodePrincipal, value: { projectId: string; turnId: string; actionId: string; actionDigest: string; fence: Fence }) {
      return transaction(db, () => {
        a.node(principal); closedObject(value, ['projectId','turnId','actionId','actionDigest','fence']);
        const projectId = id(value.projectId), turnId = id(value.turnId), actionId = id(value.actionId), actionDigest = digest(value.actionDigest);
        const turn = one("SELECT * FROM turns WHERE project_id=? AND id=? AND node_id=? AND state='admitted'", projectId, turnId, principal.nodeId);
        if (!turn) throw denied();
        const request = JSON.parse(String(turn.request_json)) as RoutingFrame; current(request);
        if (request.expiresAt <= a.now() || canonicalJson(request.fence) !== canonicalJson(value.fence)) throw stale();
        const prior = one('SELECT * FROM action_leases WHERE project_id=? AND turn_id=? AND action_id=?', projectId, turnId, actionId);
        if (prior) {
          if (prior.action_digest !== actionDigest) throw new ProtocolError(409, 'idempotency_conflict', 'Action ID already has a different digest.');
          if (Number(prior.expires_at) <= a.now()) throw stale();
          return { leaseId: String(prior.id), actionId, actionDigest, fence: request.fence, expiresAt: Number(prior.expires_at) };
        }
        const leaseId = randomUUID(), expiresAt = Math.min(request.expiresAt, a.now() + 5_000);
        run('INSERT INTO action_leases VALUES(?,?,?,?,?,?,?)', leaseId, projectId, turnId, actionId, actionDigest, canonicalJson(request.fence), expiresAt);
        return { leaseId, actionId, actionDigest, fence: request.fence, expiresAt };
      });
    },
    accept(principal: NodePrincipal, value: unknown) {
      return transaction(db, () => {
        a.node(principal);
        const f = closedObject(value, ['v','kind','eventId','projectId','sectionId','participationId','agentId','nodeId','turnId','requesterAccountId','payerAccountId','payerBindingRef','fence','sequence','expiresAt','payloadDigest','payload']);
        if (f.v !== 1 || !resultKinds.has(f.kind as FrameKind) || id(f.nodeId) !== principal.nodeId || Buffer.byteLength(canonicalJson(f)) > 65_536) throw denied();
        for (const key of ['eventId','projectId','sectionId','participationId','agentId','turnId','requesterAccountId','payerAccountId']) id(f[key]);
        existingRevision(f.sequence); time(f.expiresAt); digest(f.payloadDigest);
        const turn = one('SELECT * FROM turns WHERE project_id=? AND id=? AND node_id=?', String(f.projectId), String(f.turnId), principal.nodeId);
        if (!turn) throw new ProtocolError(404, 'resource_not_found', 'Resource not found.');
        const request = JSON.parse(String(turn.request_json)) as RoutingFrame;
        for (const key of ['projectId','sectionId','participationId','agentId','nodeId','turnId','requesterAccountId','payerAccountId','payerBindingRef','fence','expiresAt'] as const) if (canonicalJson(f[key]) !== canonicalJson(request[key])) throw denied();
        current(request);
        const frame = f as unknown as RoutingFrame;
        const resultDigest = sha256(canonicalJson(f));
        const prior = one('SELECT * FROM node_results WHERE project_id=? AND turn_id=? AND sequence=?', frame.projectId, frame.turnId!, frame.sequence);
        if (prior) {
          if (prior.digest !== resultDigest || prior.event_id !== frame.eventId) throw new ProtocolError(409, 'idempotency_conflict', 'Result sequence already has a different payload.');
          return { accepted: true as const, duplicate: true, sequence: frame.sequence };
        }
        const payload = closedObject(f.payload, f.kind === 'turn.partial' || f.kind === 'turn.final' ? ['text'] : f.kind === 'turn.failed' ? ['code'] : f.kind === 'turn.approval.request' ? ['actionId','actionDigest','requiredApproverAccountId','expiresAt'] : f.kind === 'turn.effect.receipt' ? ['actionId','actionDigest','outcome','leaseId'] : []);
        if (f.kind === 'turn.partial' || f.kind === 'turn.final') messageText(payload.text, f.kind === 'turn.partial' ? 8_000 : 16_000);
        if (f.kind === 'turn.failed' && (typeof payload.code !== 'string' || !/^[a-z0-9_]{1,80}$/.test(payload.code))) throw denied();
        if (f.kind === 'turn.approval.request' || f.kind === 'turn.effect.receipt') {
          id(payload.actionId); digest(payload.actionDigest);
          if (f.kind === 'turn.approval.request') {
            if (payload.requiredApproverAccountId !== request.payerAccountId || time(payload.expiresAt) <= a.now() || Number(payload.expiresAt) > request.expiresAt) throw denied();
          } else {
            id(payload.leaseId); if (!['succeeded','failed','unknown'].includes(String(payload.outcome))) throw denied();
            const lease = one('SELECT * FROM action_leases WHERE id=? AND project_id=? AND turn_id=? AND action_id=? AND action_digest=?', String(payload.leaseId), request.projectId, request.turnId!, String(payload.actionId), String(payload.actionDigest));
            if (!lease || lease.fence_json !== canonicalJson(request.fence)) throw denied();
          }
        }
        if (sha256(canonicalJson(payload)) !== frame.payloadDigest) throw new ProtocolError(409, 'digest_mismatch', 'Payload digest differs.');
        if (one('SELECT 1 FROM node_results WHERE project_id=? AND event_id=?', frame.projectId, frame.eventId)) throw new ProtocolError(409, 'idempotency_conflict', 'Event ID already used.');
        if (!['pending','admitted'].includes(String(turn.state)) || request.expiresAt <= a.now() || frame.sequence !== Number(turn.result_sequence) + 1) throw stale();
        if (turn.state === 'pending' && frame.kind !== 'turn.admitted' || turn.state === 'admitted' && frame.kind === 'turn.admitted') throw denied();
        const published = Number(one("SELECT COALESCE(sum(length(CAST(json_extract(frame_json,'$.payload.text') AS BLOB))),0) AS n FROM node_results WHERE project_id=? AND turn_id=?", frame.projectId, frame.turnId!)!.n);
        if (published + Buffer.byteLength(String(payload.text ?? '')) > 262_144) throw new ProtocolError(413, 'result_limit', 'Turn publication limit reached.');
        run('INSERT INTO node_results VALUES(?,?,?,?,?,?)', frame.projectId, frame.turnId!, frame.sequence, frame.eventId, resultDigest, canonicalJson(f));
        const state = frame.kind === 'turn.final' ? 'done' : frame.kind === 'turn.failed' ? 'failed' : 'admitted';
        run('UPDATE turns SET state=?,result_sequence=? WHERE project_id=? AND id=?', state, frame.sequence, frame.projectId, frame.turnId!);
        return { accepted: true as const, duplicate: false, sequence: frame.sequence };
      });
    },
    history(ctx: ProjectContext, sectionId: string) {
      a.section(ctx, sectionId);
      return { messages: all('SELECT * FROM messages WHERE project_id=? AND section_id=? ORDER BY created_at,id LIMIT 100', ctx.projectId, sectionId), results: all('SELECT r.frame_json FROM node_results r JOIN turns t ON t.project_id=r.project_id AND t.id=r.turn_id WHERE t.project_id=? AND t.section_id=? ORDER BY t.rowid,r.sequence LIMIT 100', ctx.projectId, sectionId).map(r => JSON.parse(String(r.frame_json)) as RoutingFrame) };
    },
    turn(ctx: ProjectContext, turnId: string) {
      a.member(ctx); const t = one('SELECT * FROM turns WHERE project_id=? AND id=?', ctx.projectId, id(turnId));
      if (!t) throw new ProtocolError(404, 'resource_not_found', 'Resource not found.'); a.section(ctx, String(t.section_id)); return t;
    },
  };
}

export type HubRouting = ReturnType<typeof createHubRouting>;
export type GrantSnapshot = ReturnType<HubRouting['grants']>[number];
export type ActionLease = ReturnType<HubRouting['lease']>;
export type PostMessage = Parameters<HubRouting['post']>[1];

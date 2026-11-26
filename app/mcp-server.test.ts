// Runtime cases moved to crates/minarbot-e2e/tests/e2e/rg_mcp_server.rs (#89).
// This case tests the TypeScript module app/t3/mcp-session-registry.ts.
import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { testDatabase } from './test-support/database.ts';
import { initializeMcpSessions, mintAgentSession, verifyBearer, revokeAgentSession } from './t3/mcp-session-registry.ts';

test('session registry uses exact hash lookup, bounded expiry and explicit revocation', () => {
  const db = testDatabase(':memory:');
  try {
    db.exec('CREATE TABLE agents(id TEXT PRIMARY KEY); CREATE TABLE turns(id TEXT PRIMARY KEY,agent TEXT,status TEXT)'); initializeMcpSessions(db);
    const agent = randomUUID(), turn = randomUUID(); db.prepare('INSERT INTO agents VALUES (?)').run(agent); db.prepare("INSERT INTO turns VALUES (?,?,'running')").run(turn,agent);
    const token = mintAgentSession(db,{ agent,turn,scopes:['history:read'],ttlMs:100 },1000);
    assert(verifyBearer(db,`Bearer ${token.token}`,1099)); assert(!verifyBearer(db,`Bearer ${token.token}`,1100)); assert(!verifyBearer(db,token.token,1001));
    revokeAgentSession(db,token.id,1002); assert(!verifyBearer(db,`Bearer ${token.token}`,1003));
    assert(!JSON.stringify(db.prepare('SELECT * FROM mcp_server_tokens').all()).includes(token.token));
    for (const ttlMs of [0,-1,1.5,86_400_001,Infinity]) assert.throws(() => mintAgentSession(db,{ agent,scopes:['agents:read'],ttlMs }));
  } finally { db.close(); }
});

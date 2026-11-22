import test from 'node:test';
import assert from 'node:assert/strict';
import { generateKeyPairSync, randomUUID, sign } from 'node:crypto';
import { mkdtempSync, rmSync, readFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { request as httpsRequest } from 'node:https';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createServer } from 'node:http';
import { testDatabase } from './test-support/database.ts';
import { startRustServer } from './rust-test-server.ts';
import { startApp } from './test-bridge.ts';

test('Rust JWKS rotates keys, rejects unknown signatures and enforces audience', async () => {
  const root = mkdtempSync(join(tmpdir(), 'minarbot-rust-jwks-'));
  const keys = [generateKeyPairSync('rsa', { modulusLength: 2048 }), generateKeyPairSync('rsa', { modulusLength: 2048 })];
  let rotated = false, calls = 0;
  const api = createServer((req,res) => { res.setHeader('Content-Type','application/json'); if (req.url === '/v1/jwks') { calls++; res.end(JSON.stringify({ keys: keys.slice(0,rotated?2:1).map((key,index) => ({ ...key.publicKey.export({ format:'jwk' }), kid:String(index), use:'sig', alg:'RS256' })) })); } else { res.statusCode=503; res.end('{}'); } });
  await new Promise<void>(ready => api.listen(0,'127.0.0.1',ready)); const address=api.address(); assert(address&&typeof address!=='string');
  const auth = { secretKey:'sk_test_fixture',publishableKey:'pk_test_fixture',issuer:'https://fixture.clerk.accounts.dev',origin:'https://minarbot.example',development:true,audience:'minarbot',apiUrl:`http://127.0.0.1:${address.port}` };
  const app = await startApp({ database: join(root, 'db.sqlite'), auth }, { directory: root, servers: [api] });
  const send=async (index:number,overrides:Record<string,unknown>={}) => { const now=Math.floor(Date.now()/1000); const input=`${Buffer.from(JSON.stringify({alg:'RS256',kid:String(index)})).toString('base64url')}.${Buffer.from(JSON.stringify({iss:auth.issuer,sub:'user_fixture',sid:'sess_fixture',azp:auth.origin,nbf:now-1,exp:now+60,aud:'minarbot',...overrides})).toString('base64url')}`; const jwt=`${input}.${sign('RSA-SHA256',Buffer.from(input),keys[index%2].privateKey).toString('base64url')}`; const response=await fetch(app.origin+'/api/session',{headers:{Authorization:`Bearer ${jwt}`,'Content-Type':'application/json','X-Minarbot-Protocol':'1','X-Minarbot-Request-Id':randomUUID()}}); const body=await response.json(); return {status:response.status,body}; };
  try { const first=await send(0); assert.equal(first.status,200); assert(calls>0); rotated=true; const before=calls; assert.equal((await send(1)).body.data.accountId,first.body.data.accountId); assert(calls>before); assert.equal((await send(2)).status,401); assert.equal((await send(0,{aud:'wrong'})).status,401); assert.equal((await send(0,{aud:['other','minarbot']})).status,200); }
  finally { await app.close(); api.closeAllConnections(); await new Promise<void>(ready=>api.close(()=>ready())); rmSync(root,{recursive:true,force:true}); }
});

test('Rust reopens current canonical schema and rejects unknown versions and migration drift', async () => {
  const root=mkdtempSync(join(tmpdir(),'minarbot-rust-schema-'));
  const auth={secretKey:'sk_test_fixture',publishableKey:'pk_test_fixture',issuer:'https://fixture.clerk.accounts.dev',origin:'https://minarbot.example',development:true};
  const indexes = [
    'files_scope_seq', 'occurrence_pending', 'project_workflow_notifications_reader',
    'provider_calls_turn_seq', 'queue_edit_active', 'routine_occurrences_active', 'routine_occurrences_schedule',
  ];
  const inspect = (database: string) => {
    const db = testDatabase(database, { readOnly: true });
    try {
      assert.equal(db.prepare('SELECT max(version) AS version FROM schema_migrations').get()?.version, 6);
      const schema = db.prepare("SELECT type,name,tbl_name FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name").all();
      assert.deepEqual(schema.filter(row => row.type === 'index' && indexes.includes(String(row.name))).map(row => row.name), indexes);
      return schema.map(row => ({
        ...row,
        ...(row.type === 'table' ? { columns: db.prepare('SELECT * FROM pragma_table_info(?)').all(row.name) } : {}),
        ...(row.type === 'index' ? { keys: db.prepare('SELECT * FROM pragma_index_xinfo(?)').all(row.name) } : {}),
      }));
    } finally { db.close(); }
  };
  try {
    const database=join(root,'rust-new'),app=await startRustServer({database,auth});
    try { const second=await startRustServer({database,auth}).then(async server => { await server.close(); return true; }, error => { assert.match(error.message,/Another canonical server is still running/); return false; }); assert.equal(second,false); }
    finally { await app.close(); }
    const expected = inspect(database);
    const reopened = await startRustServer({ database, auth });
    await reopened.close();
    assert.deepEqual(inspect(database), expected, 'Rust schema after restart');
    const db = testDatabase(database);
    db.prepare("INSERT INTO schema_migrations VALUES(7,'future','future',0)").run();
    db.close();
    await assert.rejects(startRustServer({ database, auth }), /Database schema is newer than this server/);
    const drift = testDatabase(database);
    drift.exec("DELETE FROM schema_migrations WHERE version=7; UPDATE schema_migrations SET checksum='bad' WHERE version=6");
    drift.close();
    await assert.rejects(startRustServer({ database, auth }), /Schema migration checksum mismatch/);
  } finally {rmSync(root,{recursive:true,force:true});}
});

test('Rust canonical TLS serves protocol errors over a verified fixture certificate', async () => {
  const root=mkdtempSync(join(tmpdir(),'minarbot-rust-tls-'));
  try {
    execFileSync('openssl',['req','-x509','-newkey','rsa:2048','-nodes','-keyout',join(root,'key'),'-out',join(root,'cert'),'-days','1','-subj','/CN=localhost','-addext','subjectAltName=IP:127.0.0.1'],{stdio:'ignore'});
    const cert=readFileSync(join(root,'cert'),'utf8'),key=readFileSync(join(root,'key'),'utf8');
    const app=await startRustServer({database:join(root,'db.sqlite'),tls:{cert,key},auth:{secretKey:'sk_test_fixture',publishableKey:'pk_test_fixture',issuer:'https://fixture.clerk.accounts.dev',origin:'https://minarbot.example'}});
    try {
      assert(app.origin.startsWith('https:'));
      const response=await new Promise<{status:number;body:string}>((ready,reject)=>{ const request=httpsRequest(app.origin+'/api/session',{ca:cert},response=>{let body='';response.setEncoding('utf8');response.on('data',chunk=>{body+=chunk;});response.on('end',()=>ready({status:response.statusCode!,body}));});request.on('error',reject);request.end(); });
      assert.equal(response.status,426);assert.deepEqual(JSON.parse(response.body),{ok:false,error:{code:'unsupported_version',message:'Unsupported protocol version.',retryable:false,requestId:''}});
    } finally {await app.close();}
  } finally {rmSync(root,{recursive:true,force:true});}
});

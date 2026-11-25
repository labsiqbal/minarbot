// Ported to crates/minarbot-e2e/tests/e2e/harness.rs (#89). Kept: the Steer
// cases pending bug B7 and the workspace-shell case for its "no approval"
// assertion pending bug B8 (docs/test-ledger.md).
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, chmodSync, rmSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { randomUUID } from 'node:crypto';
import type { DatabaseSync } from 'node:sqlite';
import { testDatabase } from './test-support/database.ts';
import { startApp } from './test-bridge.ts';
import type { HarnessRuntime } from './shared/runtime.ts';
process.env.MINARBOT_HARNESS_CANARY = 'never-inherit-this-canary';
const fakeSource = readFileSync(new URL('./fixtures/fake-harness.mjs', import.meta.url), 'utf8');
const delay = (ms: number) => new Promise(resolve => setTimeout(resolve, ms));
function executable(directory: string) { const path = join(directory,'fake-harness'); writeFileSync(path, `#!${process.execPath}\n` + fakeSource.split('\n').slice(1).join('\n')); chmodSync(path,0o700); return path; }
async function fixture(runtime: HarnessRuntime, write = true, room = false, realMcp = false) {
  const directory = mkdtempSync(join(room ? join(import.meta.dirname,'node_modules') : tmpdir(),'minarbot-harness-')), workspace = join(directory,'workspace'); mkdirSync(workspace);
  const binary = executable(directory), agent = randomUUID(), minted: {agent: string;turn: string;id: string}[] = [], revoked: string[] = [];
  if(room)mkdirSync(join(directory,'state'));
  const options = { baseURL: 'http://127.0.0.1:1/v1', database:join(directory,room?'state/db.sqlite':'db.sqlite'), port:0, timeout:10_000, workspaces:[{agent,directory:workspace,write}], harness: { executables:{codex:binary,claude:binary}, ...(!realMcp && { issuer:{ mintAgentSession(input: {agent:string;turn:string}) { const id=randomUUID(); minted.push({...input,id});return {id,token:'test-mcp-session-token'}; }, revokeAgentSession(id:string) { revoked.push(id); } } }) } };
  let app = await startApp(options);
  const request = async (path:string, body?:unknown, expected=200) => { const response=await fetch(app.origin+path,{method:body?'POST':'GET',headers:{Origin:app.origin,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});const data=await response.json();assert(expected===200?response.ok:response.status===expected,JSON.stringify(data));return data; };
  const profile = { runtime: 'api' as const, name:'Coder', model:'saved-api-model', instructions:'Help with coding.' };
  await request('/api/agents',{id:agent,...profile}); const path=`/api/agents/${agent}`;
  await request(path+'/edit',{id:randomUUID(),revision:1,...profile,runtime,confirmRuntime:true});
  const until = async (turn:string,predicate:(row:any)=>boolean) => { for(let i=0;i<600;i++){const row=(await request(path+'/turns')).find((row:any)=>row.id===turn);if(row&&predicate(row))return row;await delay(15);}assert.fail('Harness turn timed out'); };
  const send = async(text:string) => {const id=randomUUID();await request(path+'/turns',{id,text});return id;};
  return {directory,workspace,agent,path,profile,request,until,send,minted,revoked,options,get app(){return app;}, async editDatabase(edit: (db: DatabaseSync) => void){await app.close();const db=testDatabase(options.database);try{edit(db);}finally{db.close();app=await startApp(options);}},async restart(){await app.close();app=await startApp(options);},async close(){await app.close();rmSync(directory,{recursive:true,force:true});}};
}
test('claude: workspace shell inspection and checks finish without approval', { skip: process.platform !== 'linux' }, async () => {
  const f = await fixture('claude', true, false, true);
  try {
    const hostFile = join(f.directory, 'host-only.txt');
    writeFileSync(hostFile, 'private fixture');
    writeFileSync(join(f.workspace, 'fixture.test.mjs'), `
import assert from 'node:assert/strict';
import { existsSync } from 'node:fs';
assert.equal(process.cwd(), '/workspace');
assert.equal(process.env.MINARBOT_MCP_TOKEN, undefined);
assert.equal(existsSync(${JSON.stringify(hostFile)}), false);
await assert.rejects(fetch(${JSON.stringify(f.app.origin + '/api/environment')}, { signal: AbortSignal.timeout(2000) }));
`);
    const id = await f.send('WORKSPACE_SHELL_FIXTURE');
    const row = await f.until(id, row => row.harnessApprovals.length || ['done','failed'].includes(row.status));
    assert.deepEqual(row.harnessApprovals, []);
    assert.equal(row.status, 'done', row.error);
    assert.equal(row.reply, 'finished');
    assert.equal(readFileSync(join(f.workspace, 'result.txt'), 'utf8'), 'checked');
  } finally { await f.close(); }
});
for(const runtime of ['codex','claude'] as const) {
  test(`${runtime}: Steer degrades to linked Restart with partial context and fresh native session`, async () => {
    const f = await fixture(runtime); try {
      const first = await f.send('HOLD'); await f.until(first, row => !!row.partial?.text);
      const active = await f.request(f.path + '/dispatch'); assert.equal(active[0].capabilities.steer, false);
      const command = { id: randomUUID(), text: 'RESTART_FINISH', dispatchMode: 'steer', targetTurn: first };
      const receipt = await f.request(f.path + '/turns', command); assert.equal(receipt.dispatch_mode, 'restart'); assert.match(receipt.dispatch_label, /cannot steer/);
      const done = await f.until(command.id, row => ['done','failed'].includes(row.status)); assert.equal(done.status, 'done', done.error);
      assert.equal((await f.until(first, row => row.status === 'cancelled')).status, 'cancelled');
      assert.equal(done.activity.dispatch[0].source_turn, first);
      const log = readFileSync(join(f.workspace, 'fake-log.jsonl'), 'utf8').trim().split('\n').map(line => JSON.parse(line));
      const prompt = log.filter(row => row.type === 'turn').at(-1).prompt;
      assert.match(prompt, /Harness /); assert.match(prompt, /Never replay uncertain operations/);
      assert.equal(log.filter(row => runtime === 'codex' ? row.method === 'thread/resume' : row.type === 'init' && row.resume).length, 0);
      const count = f.minted.length; await f.restart(); assert.deepEqual(await f.request(f.path + '/turns', command), receipt); assert.equal(f.minted.length, count);
      const follow = await f.send('FOLLOW_RESTART_FINISH'); assert.equal((await f.until(follow, row => ['done','failed'].includes(row.status))).status, 'done');
      const later = readFileSync(join(f.workspace, 'fake-log.jsonl'), 'utf8').trim().split('\n').map(line => JSON.parse(line));
      const turns = later.filter(row => row.type === 'turn');
      assert.notEqual(turns.at(-1).session, turns.at(-2).session);
      assert.notEqual(turns.at(-1).session, turns[0].session);
      assert.equal(later.filter(row => runtime === 'codex' ? row.method === 'thread/resume' : row.type === 'init' && row.resume).length, 0);
    } finally { await f.close(); }
  });
  test(`${runtime}: addressed room Steer restarts only selected member; room Stop expires approval and fences restart`, async () => {
    const f=await fixture(runtime,true,true);try {
      const peer=await f.request('/api/agents',{runtime:'api', id:randomUUID(),name:'Peer',model:'fake',instructions:'Help.'});
      const project=await f.request('/api/projects',{id:randomUUID(),name:'Harness room',folders:[{mode:'existing',path:f.workspace}],members:[{agent:f.agent,write:true},{agent:peer.id,write:true}],confirmWrite:true});
      await f.request(`/api/agents/${peer.id}/edit`,{id:randomUUID(),revision:1,name:'Peer',model:'fake',instructions:'Help.',runtime,confirmRuntime:true});
      const path=`/api/channels/${project.sections[0].id}/turns`,root=randomUUID();
      await f.request(path,{id:root,text:'APPROVE'});
      const read=async()=>f.request(path);
      let rows:any[]=[];
      for(let n=0;n<600;n++){rows=await read();if(rows.filter(row=>row.harnessApprovals?.some((a:any)=>a.state==='approval')).length===1)break;await delay(15);}
      assert.equal(rows.filter(row=>row.harnessApprovals?.length).length,1,JSON.stringify(rows));
      const selected=rows.find(row=>row.status==='running'),other=rows.find(row=>row.status==='queued');
      assert(other, 'Peer waits in serialized lane');
      const command={id:randomUUID(),text:'RESTART_FINISH',dispatchMode:'steer',targetTurn:selected.id,recipient:selected.agent};
      const receipt=await f.request(path,command);assert.equal(receipt.dispatch_mode,'restart');
      for(let n=0;n<600;n++){rows=await read();if(rows.find(row=>row.id===command.id)?.harnessApprovals?.length)break;await delay(15);}
      const restarted=rows.find(row=>row.id===command.id),approval=restarted.harnessApprovals[0];
      await f.request(`${path}/${command.id}/harness-approvals/${approval.id}`,{id:randomUUID(),revision:approval.revision,decision:'allow'});
      for(let n=0;n<600;n++){rows=await read();if(rows.find(row=>row.id===command.id)?.status==='done')break;await delay(15);}
      assert.equal(rows.find(row=>row.id===command.id)?.status,'done',JSON.stringify(rows));
      for(let n=0;n<600;n++){rows=await read();if(rows.find(row=>row.id===other.id)?.harnessApprovals?.length)break;await delay(15);}
      assert.equal(rows.find(row=>row.id===other.id)?.status,'running');
      assert.equal(rows.find(row=>row.id===other.id)?.harnessApprovals[0].state,'approval');
      const stopPath=path.replace('/turns',`/discussions/${root}/stop`),stop={id:randomUUID(),generation:root};
      const stopped=await f.request(stopPath,stop);assert(stopped.cancelled.includes(other.id));
      for(let n=0;n<200;n++){rows=await read();if(rows.find(row=>row.id===other.id)?.harnessApprovals[0].state==='expired')break;await delay(15);}
      assert.equal(rows.find(row=>row.id===other.id)?.harnessApprovals[0].state,'expired');
      const card=rows.find(row=>row.id===other.id).harnessApprovals[0];await f.request(`${path}/${other.id}/harness-approvals/${card.id}`,{id:randomUUID(),revision:card.revision,decision:'allow'},409);
      const count=f.minted.length;await f.restart();assert.deepEqual(await f.request(stopPath,stop),stopped);assert.deepEqual(await f.request(path,command),receipt);assert.equal(f.minted.length,count);
    }finally{await f.close();}
  });
}

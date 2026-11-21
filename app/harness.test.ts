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
  test(`${runtime}: native check receipt prevents a second verification turn`, async () => {
    const f = await fixture(runtime);
    try {
      const id = await f.send('Write a result NATIVE_CHECK'), done = await f.until(id, row => ['done','failed'].includes(row.status));
      assert.equal(done.status, 'done', done.error);
      assert.equal(done.activity.artifacts.filter((a: any) => a.kind === 'check').length, 1);
      assert(done.activity.harness.some((step: any) => step.name === (runtime === 'codex' ? 'fileChange' : 'Write') && step.state === 'completed'));
      const log = readFileSync(join(f.workspace, 'fake-log.jsonl'), 'utf8').trim().split('\n').map(line => JSON.parse(line));
      assert.equal(log.filter(row => row.type === 'turn').length, 1);
      assert(!done.activity.harness.some((step: any) => step.name === 'Verify result'));
      const contexts = log.filter(row => row.context).map(row => JSON.stringify(row.context));
      assert(contexts.every(text => text.includes(`through the ${runtime} runtime`)));
      assert(contexts.every(text => text.includes('read_routine to verify saved fields')));
    } finally { await f.close(); }
  });
  test(`${runtime}: gated segments replace only final answer, coalesce writes and restore without replay`, async () => {
    const f = await fixture(runtime);
    try {
      await f.editDatabase(db => db.exec('CREATE TABLE snapshot_writes(at INTEGER); CREATE TRIGGER count_snapshot_insert AFTER INSERT ON provider_streams BEGIN INSERT INTO snapshot_writes VALUES (1); END; CREATE TRIGGER count_snapshot_update AFTER UPDATE ON provider_streams BEGIN INSERT INTO snapshot_writes VALUES (1); END;'));
      const start = Date.now(), id = await f.send('STREAM_GATE');
      const partial = await f.until(id, row => row.partial?.text === 'Draft ' + 'x'.repeat(100));
      assert.equal(partial.status, 'running'); assert.equal(partial.partial.final, false);
      const segments = partial.activity.stream;
      assert.deepEqual(segments.map((item: any) => [item.ordinal, item.kind]), [[0, 'text'], [1, 'tool'], [2, 'text']]);
      assert.equal(segments[0].text, 'Before tool. ' + '🧪'.repeat(100) + '[REDACTED]');
      assert(!JSON.stringify(partial).includes('test-mcp-session-token'));
      assert.equal(partial.partial.segment, segments[2].id);
      const restored = (await f.request(f.path + '/turns')).find((row: any) => row.id === id);
      assert.deepEqual(restored.partial, partial.partial); assert.equal(f.minted.length, 1);
      writeFileSync(join(f.workspace, 'stream-release'), '');
      const done = await f.until(id, row => row.status === 'done');
      assert.equal(done.reply, 'Final answer.'); assert.equal(done.partial, null);
      assert.deepEqual(done.activity.stream.map((item: any) => [item.ordinal, item.id]), segments.map((item: any) => [item.ordinal, item.id]));
      assert.equal(done.activity.stream[2].text, done.reply); assert.equal(done.activity.stream[2].final, 1);
      let writes = 0;
      const elapsed = Date.now() - start;
      await f.editDatabase(db => { writes = Number(db.prepare('SELECT count(*) AS n FROM snapshot_writes').get()!.n); });
      assert(writes <= Math.ceil(elapsed / 40) + 8 && writes < 30, `Token-rate writes: ${writes}`);
      await f.restart();
      const final = (await f.request(f.path + '/turns')).find((row: any) => row.id === id);
      assert.equal(final.reply, 'Final answer.'); assert.deepEqual(final.activity.stream, done.activity.stream); assert.equal(f.minted.length, 1);
    } finally { await f.close(); }
  });
  test(`${runtime}: abort flush preserves final pending segment and stays unfinished after restart`, async () => {
    const f = await fixture(runtime);
    try {
      const id = await f.send('STREAM_GATE');
      await f.until(id, row => row.partial?.text === 'Draft ' + 'x'.repeat(100));
      await f.request(`${f.path}/turns/${id}/cancel`, { id: randomUUID() });
      await f.until(id, row => row.status === 'cancelled');
      for (let n = 0; n < 100 && !f.revoked.length; n++) await delay(10);
      await f.restart();
      const row = (await f.request(f.path + '/turns')).find((row: any) => row.id === id);
      assert.equal(row.partial.text, 'Draft ' + 'x'.repeat(100)); assert.equal(row.partial.final, false);
      assert.equal(row.partial.status, 'failed'); assert.equal(f.minted.length, 1); assert.equal(f.revoked.length, 1);
    } finally { await f.close(); }
  });
  test(`${runtime}: error flush commits buffered tail without declaring a final answer`, async () => {
    const f = await fixture(runtime);
    try {
      const id = await f.send('STREAM_CRASH');
      await f.until(id, row => row.partial?.text === 'Draft ' + 'x'.repeat(100));
      writeFileSync(join(f.workspace, 'stream-release'), '');
      const failed = await f.until(id, row => row.status === 'failed');
      assert.equal(failed.partial.text, 'Draft ' + 'x'.repeat(100) + ' buffered tail'); assert.equal(failed.partial.final, false);
      assert.equal(failed.partial.status, 'failed'); assert.equal(failed.activity.stream.at(-1).final, 0);
      await f.restart();
      assert.deepEqual((await f.request(f.path + '/turns')).find((row: any) => row.id === id).partial, failed.partial);
      assert.equal(f.minted.length, 1); assert.equal(f.revoked.length, 1);
    } finally { await f.close(); }
  });
  test(`${runtime}: authoritative snapshot and reply apply identical session-token redaction`, async () => {
    const f = await fixture(runtime);
    try {
      const id = await f.send('FINAL_SECRET'), done = await f.until(id, row => row.status === 'done');
      assert.equal(done.reply, 'finished [REDACTED]');
      assert.equal(done.activity.stream.at(-1).text, done.reply); assert(!JSON.stringify(done).includes('test-mcp-session-token'));
    } finally { await f.close(); }
  });
  test(`${runtime}: stream, tool activity, MCP lifetime, follow-up and restart resume`,async()=>{
    const f=await fixture(runtime);
    const streamController = new AbortController();
    try {
      const stream = await fetch(f.app.origin + f.path + '/turns/stream', { signal: streamController.signal });
      const reader = stream.body!.getReader();
      const first = await f.send('FIRST_PARTIAL_GATE');
      const partial = await f.until(first, row => row.partial?.text || ['done', 'failed', 'cancelled'].includes(row.status));
      assert.equal(partial.status, 'running', partial.error);
      assert.equal(partial.partial.text, 'Harness ');
      try {
        const frame = new TextDecoder().decode((await reader.read()).value);
        assert(frame.startsWith('data: '));
        const snapshot = JSON.parse(frame.split('\n\n')[0].slice(6));
        assert.equal(snapshot.id, first); assert.equal(snapshot.partial.text, ''); assert.equal(snapshot.partial.seq, partial.partial.seq);
      } finally { streamController.abort(); }
      writeFileSync(join(f.workspace, 'stream-release'), '');

      const done=await f.until(first,row=>['done','failed'].includes(row.status));assert.equal(done.status,'done',done.error);assert.equal(done.reply,'finished');assert(done.activity.harness.length);assert.equal(done.activity.harness[0].state,'completed');
      const second=await f.send('SECOND');assert.equal((await f.until(second,row=>['done','failed'].includes(row.status))).status,'done');
      await f.restart();const third=await f.send('THIRD');assert.equal((await f.until(third,row=>['done','failed'].includes(row.status))).status,'done');
      const log=readFileSync(join(f.workspace,'fake-log.jsonl'),'utf8');assert(runtime==='codex'?log.includes('thread/resume'):log.includes('"resume":true'));assert.equal(f.minted.length,3);assert.deepEqual(f.revoked,f.minted.map(row=>row.id));
      const contexts = log.trim().split('\n').map(line => JSON.parse(line)).filter(row => row.context).map(row => typeof row.context === 'string' ? row.context : Object.values(row.context).map((entry: any) => entry.value).join(''));
      assert(contexts.length >= 3);
      for (const context of contexts) { assert.match(context, /<runtime_info>/); assert.match(context, /open_file_in_panel/); assert.doesNotMatch(context, /computer_screenshot/); assert.match(context, /run_shell/); }
      const usage=await f.request('/api/usage');assert(usage.calls.some((call:any)=>call.input_tokens===11&&call.output_tokens===7));
    } finally {streamController.abort();await f.close();}
  });
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
  test(`${runtime}: durable approval allow, deny, prefix grant and scoped receipt`,async()=>{
    const f=await fixture(runtime);try {
      async function approval(decision:string){const id=await f.send('APPROVE'),row=await f.until(id,row=>row.harnessApprovals?.length||row.status==='failed');assert.equal(row.status,'running',row.error);const card=row.harnessApprovals[0];assert.equal(card.prefix,'echo hello');const body={id:randomUUID(),revision:card.revision,decision};await f.request(`${f.path}/turns/${id}/harness-approvals/${card.id}`,body);await f.request(`${f.path}/turns/${id}/harness-approvals/${card.id}`,body);const done=await f.until(id,row=>['done','failed'].includes(row.status));assert.equal(done.status,'done',done.error);assert.equal(done.reply,decision==='deny'?'denied':'finished');return id;}
      await approval('deny');await approval('allow');await approval('always');const fourth=await f.send('APPROVE');const fourthDone=await f.until(fourth,row=>['done','failed'].includes(row.status));assert.equal(fourthDone.status,'done',fourthDone.error);assert.equal(fourthDone.harnessApprovals.length,0);
      const peer=await f.request('/api/mcp-server/peer-tokens',{id:randomUUID(),name:'Peer',scopes:['agents:message']});
      const receipt=await fetch(f.app.origin+'/mcp',{method:'POST',headers:{Authorization:`Bearer ${peer.token}`,'Content-Type':'application/json',Accept:'application/json, text/event-stream'},body:JSON.stringify({jsonrpc:'2.0',id:1,method:'tools/call',params:{name:'send_message',arguments:{requestId:randomUUID(),recipient:f.agent,text:'APPROVE peer work'}}})}).then(response=>response.json());
      const delivery=receipt.result.structuredContent,thread=`/api/threads/${delivery.thread}/turns`;
      let peerTurn:any;for(let n=0;n<600;n++){peerTurn=(await f.request(thread))[0];if(peerTurn?.harnessApprovals?.length)break;assert(!['done','failed'].includes(peerTurn?.status),'Peer bypassed saved command approval');await delay(15);}
      assert.equal(peerTurn.harnessApprovals[0].state,'approval');const peerCard=peerTurn.harnessApprovals[0];
      await f.request(`${thread}/${delivery.turn}/harness-approvals/${peerCard.id}`,{id:randomUUID(),revision:peerCard.revision,decision:'deny'});
      for(let n=0;n<600;n++){peerTurn=(await f.request(thread))[0];if(peerTurn.status==='done')break;await delay(15);}assert.equal(peerTurn.reply,'denied');
      const secret=await f.send('APPROVE_SECRET'),row=await f.until(secret,row=>row.harnessApprovals?.length),card=row.harnessApprovals[0];
      assert.equal(card.prefix,null);assert(!JSON.stringify(card).includes('test-mcp-session-token'));assert.match(card.command,/\[redacted\]/);
      await f.request(`${f.path}/turns/${secret}/harness-approvals/${card.id}`,{id:randomUUID(),revision:card.revision,decision:'always'},400);
      await f.request(`${f.path}/turns/${secret}/harness-approvals/${card.id}`,{id:randomUUID(),revision:card.revision,decision:'deny'});
      assert.equal((await f.until(secret,row=>['done','failed'].includes(row.status))).reply,'denied');
      assert.equal(f.minted.length,6);assert.equal(f.revoked.length,6);
    } finally {await f.close();}
  });
  test(`${runtime}: workspace write policy, outside-file and network consent`,async()=>{
    const f=await fixture(runtime);try{
      const inside=await f.send('WORKSPACE_WRITE');const done=await f.until(inside,row=>['done','failed'].includes(row.status));assert.equal(done.status,'done',done.error);assert.equal(done.harnessApprovals.length,0);
      for(const [text,kind] of [['OUTSIDE_FILE','file'],['NETWORK','network']]){
        const id=await f.send(text),row=await f.until(id,row=>row.harnessApprovals?.length||row.status==='failed');assert.equal(row.status,'running',row.error);const card=row.harnessApprovals[0];assert.equal(card.kind,kind);assert.equal(card.prefix,null);
        await f.request(`${f.path}/turns/${id}/harness-approvals/${card.id}`,{id:randomUUID(),revision:card.revision,decision:'always'},400);
        await f.request(`${f.path}/turns/${id}/harness-approvals/${card.id}`,{id:randomUUID(),revision:card.revision,decision:'deny'});
        assert.equal((await f.until(id,row=>['done','failed'].includes(row.status))).reply,'denied');
      }
    }finally{await f.close();}
    const read=await fixture(runtime,false);try{
      const id=await read.send('READ_ONLY_WRITE'),row=await read.until(id,row=>row.harnessApprovals?.length||row.status==='failed');assert.equal(row.status,'running',row.error);const card=row.harnessApprovals[0];assert.equal(card.kind,'file');
      await read.request(`${read.path}/turns/${id}/harness-approvals/${card.id}`,{id:randomUUID(),revision:card.revision,decision:'allow'});assert.equal((await read.until(id,row=>['done','failed'].includes(row.status))).status,'done');
    }finally{await read.close();}
  });
  test(`${runtime}: crash fails once, preserves unfinished text and never replays`,async()=>{
    const f=await fixture(runtime);try {const id=await f.send('CRASH'),row=await f.until(id,row=>row.status==='failed');assert.match(row.error,/stopped before completion|could not start/);assert.equal(row.partial.text,'Harness ');await f.restart();await delay(100);assert.equal(f.minted.length,1);assert.equal(f.revoked.length,1);}finally{await f.close();}
  });
  test(`${runtime}: cancel kills descendants and revokes MCP; mid-turn runtime switch refused`,async()=>{
    const f=await fixture(runtime);try {const id=await f.send('HOLD');await f.until(id,row=>row.partial?.text);await f.request(f.path+'/edit',{id:randomUUID(),revision:2,...f.profile,runtime:'api',confirmRuntime:true},409);await f.request(`${f.path}/turns/${id}/cancel`,{id:randomUUID()});await f.until(id,row=>row.status==='cancelled');for(let i=0;i<200&&!f.revoked.length;i++)await delay(10);assert.equal(f.revoked.length,1);const log=readFileSync(join(f.workspace,'fake-log.jsonl'),'utf8').trim().split('\n').map(line=>JSON.parse(line));const descendant=log.find(row=>row.type==='descendant');assert(descendant);let alive=true;try{const state=readFileSync(`/proc/${descendant.pid}/stat`,'utf8');alive=!state.includes(') Z ');}catch{alive=false;}assert.equal(alive,false,'Harness descendant survived cancel');}finally{await f.close();}
  });
  test(`${runtime}: pending approval expires on shutdown/restart, no auto approval`,async()=>{
    const f=await fixture(runtime);try {const id=await f.send('APPROVE'),row=await f.until(id,row=>row.harnessApprovals?.length);const card=row.harnessApprovals[0];await f.restart();const expired=(await f.request(f.path+'/turns')).find((row:any)=>row.id===id);assert.equal(expired.harnessApprovals[0].state,'expired');assert.equal(f.revoked.length,1);await f.request(`${f.path}/turns/${id}/harness-approvals/${card.id}`,{id:randomUUID(),revision:card.revision,decision:'allow'},409);assert.equal(f.minted.length,1);}finally{await f.close();}
  });
  test(`${runtime}: bounded output fails without replay`,async()=>{const f=await fixture(runtime);try{const id=await f.send('OVERSIZE'),row=await f.until(id,row=>row.status==='failed');assert.match(row.error,/output line exceeded/);assert.equal(f.minted.length,1);assert.equal(f.revoked.length,1);}finally{await f.close();}});
}
test('runtime changes require confirmation and revision; missing workspaces are provisioned',async()=>{
  const f=await fixture('codex');try{await f.request(f.path+'/edit',{id:randomUUID(),revision:2,...f.profile,runtime:'api'},409);await f.request(f.path+'/edit',{id:randomUUID(),revision:1,...f.profile,runtime:'api',confirmRuntime:true},409);const command={id:randomUUID(),revision:2,...f.profile,runtime:'api',confirmRuntime:true};await f.request(f.path+'/edit',command);await f.request(f.path+'/edit',command);const directory=await f.request('/api/agents');assert.equal(directory.agents[0].runtime,'api');assert.equal(directory.agents[0].revision,3);const other=await f.request('/api/agents',{id:randomUUID(),...f.profile});const choices=await f.request(`/api/agents/${other.id}/runtimes`);assert(choices.choices.every((choice:any)=>choice.enabled===choice.available));await f.request(`/api/agents/${other.id}/edit`,{id:randomUUID(),revision:1,...f.profile,runtime:'codex',confirmRuntime:true});rmSync(f.options.harness.executables.codex);
    await f.restart();
    const missing = (await f.request('/api/runtimes')).choices.find((choice: { runtime: string }) => choice.runtime === 'codex');
    assert.equal(missing.available, false);
    assert.equal(missing.version, null);}finally{await f.close();}
});

for (const runtime of ['codex','claude'] as const) test(`${runtime}: done check nudges once and preserves native session`, async () => {
  const f = await fixture(runtime);
  try {
    const id = await f.send('Create a deliverable.');
    await f.until(id, row => row.status === 'done');
    const rows = readFileSync(join(f.workspace,'fake-log.jsonl'),'utf8').trim().split('\n').map(line => JSON.parse(line));
    const turns = rows.filter(row => row.type === 'turn');
    assert.equal(turns.length,2); assert.match(turns[1].prompt,/One verification reminder/);
    assert.equal(turns[0].session,turns[1].session);
    if(runtime==='claude') assert.equal(rows.find(row=>row.type==='stop-hook').decision,'block');
    if(runtime==='codex') { const usage = await f.request('/api/usage'); assert(usage.calls.some((call:any)=>call.input_tokens===22&&call.output_tokens===14)); }
  } finally { await f.close(); }
});

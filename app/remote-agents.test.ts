import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { startApp } from './test-bridge.ts';
import { restoreRemoteMessage } from './src/remote-message-command.ts';

type App=Awaited<ReturnType<typeof startApp>>;
test('remote message recovery keeps exact target, ID and payload and rejects corrupt storage',()=>{
  const machine=randomUUID(),agent=randomUUID(),command={id:randomUUID(),machine,agent,text:'Keep this request.'};
  assert.equal(restoreRemoteMessage(null,machine,agent),null);
  assert.deepEqual(restoreRemoteMessage(JSON.stringify(command),machine,agent),command);
  for(const raw of ['{bad','null','[]',JSON.stringify({...command,machine:randomUUID()}),JSON.stringify({...command,agent:randomUUID()}),JSON.stringify({...command,id:'bad'}),JSON.stringify({...command,text:' '.repeat(16)}),JSON.stringify({...command,text:'x'.repeat(16_001)}),JSON.stringify({...command,extra:true})])assert.throws(()=>restoreRemoteMessage(raw,machine,agent));
});
async function api(app:App,path:string,body?:unknown,status=200):Promise<any>{const r=await fetch(app.origin+path,{method:body===undefined?'GET':'POST',headers:{Origin:app.origin,'Content-Type':'application/json',Connection:'close'},body:body===undefined?undefined:JSON.stringify(body)});const value=await r.json();assert.equal(r.status,status,JSON.stringify(value));return value;}
async function until(check:()=>Promise<any>){const end=Date.now()+20000;while(Date.now()<end){const result=await check();if(result)return result;await new Promise(r=>setTimeout(r,30));}assert.fail('Remote fixture timed out');}
async function fixture(script:(body:any)=>any,limits={}){
  const dir=mkdtempSync(join(tmpdir(),'minarbot-remote-agents-'));
  const provider=createServer(async(req,res)=>{let raw='';for await(const c of req)raw+=c;try{const result=await script(JSON.parse(raw)),message=typeof result==='string'?{content:result}:{tool_calls:[{id:randomUUID(),type:'function',function:{name:result.name,arguments:JSON.stringify(result.args)}}]};res.setHeader('Content-Type','application/json');res.end(JSON.stringify({choices:[{finish_reason:typeof result==='string'?'stop':'tool_calls',message}]}));}catch{res.writeHead(500);res.end('{}');}});
  await new Promise<void>(r=>provider.listen(0,'127.0.0.1',r));const address=provider.address();assert(address&&typeof address!=='string');
  const common={baseURL:`http://127.0.0.1:${address.port}/v1`,agentLimits:limits}, aOptions={...common,database:join(dir,'a.sqlite')},bOptions={...common,database:join(dir,'b.sqlite')};
  let a = await startApp(aOptions, { directory: dir, servers: [provider] }), b: App;
  const close = async () => {
    await a.close().catch(() => {});
    await b?.close().catch(() => {});
    provider.closeAllConnections();
    await new Promise<void>(resolve => provider.close(() => resolve()));
    rmSync(dir, { recursive: true, force: true });
  };
  b = await startApp(bOptions).catch(async error => { await close(); throw error; });
  await api(a,'/api/environment',{id:randomUUID(),name:'A'});await api(b,'/api/environment',{id:randomUUID(),name:'B'});
  const alpha=await api(a,'/api/agents',{runtime:'api', id:randomUUID(),name:'Alpha',model:'Alpha',instructions:'Help.'},201),beta=await api(b,'/api/agents',{runtime:'api', id:randomUUID(),name:'Beta',role:'Personal agent',model:'Beta',instructions:'Help.'},201);
  const code=await api(b,'/api/pairing-codes',{id:randomUUID()});const machine=await api(a,'/api/machines',{id:randomUUID(),origin:b.origin,code:code.code});
  return {dir,alpha,beta,machine,get a(){return a;},get b(){return b;},aOptions,bOptions,async restartA(){const port=Number(new URL(a.origin).port);await a.close();a=await startApp({...aOptions,port});},async restartB(){const port=Number(new URL(b.origin).port);await b.close();b=await startApp({...bOptions,port});},close};
}
const tools=(body:any)=>body.messages.filter((row:any)=>row.role==='tool').map((row:any)=>JSON.parse(row.content));



// Kept for B1 (docs/test-ledger.md): the strict model-request count. The Rust port
// (pr_remote_agents::delegation_depth_travels_across_machines_and_stops_ping_pong)
// leaves out the extra step after each paused delegation.
test('delegation depth travels across machines and stops ping-pong',async()=>{
  let requests=0;
  const f=await fixture(body=>{requests++;const rs=tools(body);if(rs.length)return JSON.stringify(rs.at(-1));return {name:'delegate_task',args:{agent:body.model==='Alpha'?'Beta@B':'Alpha@A',task:'Continue',wait:true}};},{depth:2,rate:10});
  try{await f.b.refreshMachines();const id=randomUUID(),path=`/api/agents/${f.alpha.id}/turns`;await api(f.a,path,{id,text:'Start loop'},202);const row=await until(async()=>{const rows=await api(f.a,path);return rows.find((r:any)=>r.id===id&&r.status==='done');});assert.match(row.reply,/depth/);assert.equal(requests,6);}
  finally{await f.close();}
});

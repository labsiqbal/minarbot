import test from 'node:test';
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {randomUUID} from 'node:crypto';
import type { DatabaseSync } from 'node:sqlite';
import { testDatabase } from './test-support/database.ts';
import {mkdtempSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {startApp} from './test-bridge.ts';
import {localScript,LocalAgentError} from './test-support/script-client.ts';
import { artifactReceiver, rustConnectorFixture, slackEnvelope, telegramUpdate, waitFor } from './test-support/rust-connector-fixture.ts';

for(const platform of ['slack','telegram'] as const)for(const mode of ['step','reply','notice','nested'])test(`${platform} ${mode} workflow artifacts are scoped to owned run and retain partial bytes`,async()=>{
 const root=mkdtempSync(join(tmpdir(),'minarbot-workflow-files-')),owner=randomUUID(),worker=randomUUID(),teammate=randomUUID(),grandchild=randomUUID(),foreign=randomUUID(),text='workflow artifact exact';let db: DatabaseSync | undefined, sequence=0, previousFile='';const uploads:Buffer[]=[];
 const provider=createServer(async(req,res)=>{let raw='';for await(const part of req)raw+=part;const data=JSON.parse(raw),done=data.messages.some((m:any)=>m.role==='tool');res.end(JSON.stringify({choices:[{finish_reason:done?'stop':'tool_calls',message:done?{content:'Done'}:{tool_calls:[{id:'file',type:'function',function:['delegate','nested','notice'].includes(data.model)?{name:'send_message',arguments:JSON.stringify({recipients:[data.model==='nested'?grandchild:teammate],text:'Make file',expects_reply:data.model!=='notice'})}:{name:'create_text_file',arguments:JSON.stringify({name:'workflow.txt',text})}}]}}]}));});await new Promise<void>(r=>provider.listen(0,'127.0.0.1',r));const address=provider.address();assert(address&&typeof address!=='string');const scriptToken='connector-artifact-test-token-0123456789';const app=await startApp({database:join(root,'app.sqlite'),baseURL:`http://127.0.0.1:${address.port}/v1`,scriptToken,scriptDirectory:join(root,'scripts')}, { directory: root, servers: [provider] });
 const actual=localScript(join(root,'scripts'));
 const connector=await rustConnectorFixture(platform,root,owner,app.origin,scriptToken,artifactReceiver(platform,uploads));
 const journal=join(connector.directory,platform+'.sqlite');
 const outbox=()=>db!.prepare(`SELECT * FROM ${platform}_outbox ORDER BY rowid`).all();
 const deliver=()=>waitFor(()=>{connector.check();return outbox().every(row=>row.status==='sent');},'Artifacts and text were not delivered');
 const restart=async()=>{await connector.stop();db!.close();db=testDatabase(journal);connector.start();};
 async function ui(path:string,body?:unknown):Promise<any>{const res=await fetch(app.origin+path,{method:body?'POST':'GET',headers:{Origin:app.origin,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});const value=await res.json();assert(res.ok,JSON.stringify(value));return value;}
 async function incoming(text:string){
  const id=++sequence;
  await connector.send(platform==='slack'?slackEnvelope(id,text):telegramUpdate(id,text));
  db??=testDatabase(journal);
  return db.prepare(`SELECT * FROM ${platform}_inbox ORDER BY rowid DESC LIMIT 1`).get()!;
 }
 try{
  for(const id of [owner,worker,teammate,grandchild,foreign])await ui('/api/agents',{runtime:'api', id,name:id,model:mode!=='step'&&id===worker?(mode==='notice'?'notice':'delegate'):mode==='nested'&&id===teammate?'nested':'fixture',instructions:'Help'});for(const id of [owner,foreign])await ui(`/api/agents/${id}/scripts`,{id:randomUUID(),revision:0,enabled:true});
  connector.start();
  const readFault={method:'GET',path:/\/files\/[^/?]+$/,after:true};
  const metadataFault={method:'GET',path:/\/files\/[^/?]+\?metadata=1$/,after:false};
  const waitDone=(turn:unknown)=>waitFor(()=>{connector.check();return db!.prepare(`SELECT status FROM ${platform}_inbox WHERE turn=?`).get(String(turn))?.status==='done';},'Connector intake did not settle');
  for(const partial of mode==='notice'?[false]:[false,true]){
   const workflow=await ui(`/api/agents/${owner}/workflows`,{id:randomUUID(),revision:0,name:'Files',graph:{entry:'work',maxSteps:3,nodes:[{id:'work',kind:'agent',agent:worker,prompt:'Make file',next:partial?'ask':'out'},...(partial?[{id:'ask',kind:'approval',prompt:'Continue?',yes:'out'}]:[]),{id:'out',kind:'output'}]}});
   if(mode!=='notice'&&!partial)connector.faults.push(readFault,metadataFault);
   const row=await incoming(`${platform==='slack'?'!':'/'}workflow ${workflow.id} 1 Work`);
   if(partial){
    const run=await waitFor(async()=>{
     const response=await fetch(`${app.origin}/api/workflow-runs/${row.turn}`);
     if(response.status===404)return;
     assert(response.ok);
     const result=await response.json();
     return result.status==='approval'&&result;
    },'Workflow did not reach approval');
    await ui(`/api/workflow-runs/${row.turn}/cancel`,{id:randomUUID(),revision:run.revision});
   }
   if(mode!=='notice'&&!partial){
    await connector.expectedFailure(/Local agent request failed/);assert.equal(uploads.length,0);connector.start();
    await connector.expectedFailure(/Local agent request failed/);assert.equal(uploads.length,0);
    assert.equal(db!.prepare(`SELECT count(*) AS n FROM ${platform}_document_workflows`).get()!.n,1);connector.start();
   }
   await waitDone(row.turn);await deliver();
   const path=`/api/scripts/agents/${owner}/workflow-runs/${row.turn}`,run=await actual(path);
   if(mode==='step'&&!partial){
    assert.equal(run.status,'done');assert.equal(run.result,'Done');
    assert(connector.jsonPosts<{text:string}>(platform==='slack'?'chat.postMessage':'sendMessage').some(post=>post.text==='Done'));
   }
   if(mode==='notice'){
    const detail=await ui(`/api/workflow-runs/${row.turn}`),step=detail.steps.find((step:any)=>step.thread),parent=(await ui(`/api/threads/${step.thread}/turns`))[0],child=parent.agentMessages[0];let childTurn:any;
    for(let i=0;i<200;i++){childTurn=(await ui(`/api/threads/${child.thread}/turns`))[0];if(childTurn.status==='done')break;await new Promise(r=>setTimeout(r,10));}
    assert.equal(childTurn.status,'done');assert.equal(childTurn.generatedFiles.length,1);assert.deepEqual((await actual(path)).generatedFiles,[]);assert.equal(uploads.length,0);
    await assert.rejects(actual(`${path}/files/${childTurn.generatedFiles[0].id}`),(error:any)=>error.status===404);
    await ui(`/api/workflows/${workflow.id}/delete`,{id:randomUUID(),revision:1});continue;
   }
   assert.equal(run.generatedFiles.length,1);assert.equal(uploads.at(-1)!.toString(),text);const file=run.generatedFiles[0];assert.equal((await actual(`${path}/files/${file.id}`,undefined,undefined,undefined,true)).toString(),text);
   await assert.rejects(actual(`/api/scripts/agents/${foreign}/workflow-runs/${row.turn}/files/${file.id}`),(error:any)=>error instanceof LocalAgentError&&error.status===404);await assert.rejects(actual(`${path}/files/${randomUUID()}`),(error:any)=>error.status===404);
   if(previousFile)await assert.rejects(actual(`${path}/files/${previousFile}`),(error:any)=>error.status===404);previousFile=file.id;
   if(partial)assert(connector.posts.filter(s=>s.method===(platform==='slack'?'files.completeUploadExternal':'sendDocument')).at(-1)!.raw.includes('Partial result'));
   const count=uploads.length;await restart();const probe=await incoming(platform==='slack'?'!status':'/status');await waitDone(probe.turn);await deliver();assert.equal(uploads.length,count);
   if(!partial&&['reply','nested'].includes(mode)){
    const detail=await ui(`/api/workflow-runs/${row.turn}`),step=detail.steps.find((step:any)=>step.thread),parent=(await ui(`/api/threads/${step.thread}/turns`))[0],child=parent.agentMessages[0];
    const side=(await ui(`/api/agents/${teammate}/threads`)).find((thread:any)=>thread.id===child.thread);
    const archived=await ui(`/api/threads/${child.thread}/edit`,{id:randomUUID(),revision:side.revision,name:side.name,archived:true});await ui(`/api/threads/${child.thread}/delete`,{id:randomUUID(),revision:archived.revision});
    await assert.rejects(actual(`${path}/files/${file.id}`),(error:any)=>error.status===404);assert.deepEqual((await actual(path)).generatedFiles,[]);
   }
   if(partial){const detail=await ui(`/api/workflow-runs/${row.turn}`),threadId=detail.steps.find((step:any)=>step.thread).thread;const side=(await ui(`/api/agents/${worker}/threads`)).find((thread:any)=>thread.id===threadId);const archived=await ui(`/api/threads/${threadId}/edit`,{id:randomUUID(),revision:side.revision,name:side.name,archived:true});await ui(`/api/threads/${threadId}/delete`,{id:randomUUID(),revision:archived.revision});await assert.rejects(actual(`${path}/files/${file.id}`),(error:any)=>error.status===404);assert.deepEqual((await actual(path)).generatedFiles,[]);}
   await ui(`/api/workflows/${workflow.id}/delete`,{id:randomUUID(),revision:1});await assert.rejects(actual(`${path}/files/${file.id}`),(error:any)=>error.status===404);
  }
  assert.deepEqual(await ui(`/api/agents/${owner}/turns`),[]);await ui(`/api/agents/${owner}/scripts`,{id:randomUUID(),revision:1,enabled:false});await assert.rejects(actual(`/api/scripts/agents/${owner}/workflow-runs/${randomUUID()}`),(error:any)=>error.status===403);
 }finally{await connector.close();db?.close();await app.close();provider.closeAllConnections();await new Promise<void>(r=>provider.close(()=>r()));rmSync(root,{recursive:true,force:true});}
});

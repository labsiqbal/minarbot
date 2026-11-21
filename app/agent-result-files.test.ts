import test from 'node:test';
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {randomUUID} from 'node:crypto';
import {mkdtempSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {startApp} from './test-bridge.ts';
import {pdfFixture} from './test-support/pdf-fixture.ts';
import {setTimeout as delay} from 'node:timers/promises';

for(const pdf of [false,true]) test(`requester reads returned teammate ${pdf?'PDF':'text'} artifacts but not private inputs or no-reply outputs`,async()=>{
 const dir=mkdtempSync(join(tmpdir(),'minarbot-result-read-'));let worker='',created='',otherOutput='',target='',expectsReply=true,restart=false,hold=false,release:(()=>void)|undefined,providerReady:(()=>void)|undefined;
 const results:any[]=[],returned:any[]=[];let creates=0,openReturned=false;
 const provider=createServer(async(req,res)=>{
  let raw='';for await(const chunk of req)raw+=chunk;const body=JSON.parse(raw),tools=body.messages.filter((m:any)=>m.role==='tool'),last=tools.length?JSON.parse(tools.at(-1).content):null;let fn:any,answer='Done';
  if(body.model==='worker'){
   if(!last){creates++;fn={name:'create_text_file',arguments:JSON.stringify({name:pdf?'result.pdf':'result.txt',text:pdf?pdfFixture('REQUESTED_OUTPUT',1,false).toString('utf8'):'REQUESTED_OUTPUT'})};}
   else created=last.id;
  }else if(!last)fn={name:'send_message',arguments:JSON.stringify({recipients:[worker],text:'Make file',expects_reply:expectsReply})};
  else if(last.request){
   returned.push(last);if(restart){restart=false;res.writeHead(503);res.end();return;}
   if(hold)await new Promise<void>(resolve=>{release=resolve;providerReady?.();});
   for(let i=0;i<200&&!created;i++)await new Promise(r=>setTimeout(r,5));
   fn={name:openReturned?'open_file_in_panel':'read_attachment',arguments:JSON.stringify({id:target||created})};
  }else{results.push(last);answer=JSON.stringify(last);}
  res.end(JSON.stringify({choices:[{finish_reason:fn?'tool_calls':'stop',message:fn?{tool_calls:[{id:fn.name,type:'function',function:fn}]}:{content:answer}}]}));
 });
 await new Promise<void>(r=>provider.listen(0,'127.0.0.1',r));const address=provider.address();assert(address&&typeof address!=='string');
 let startPdf!:()=>void,releasePdf!:()=>void;
 const pdfStarted=new Promise<void>(resolve=>{startPdf=resolve;});
 const pdfReleased=new Promise<void>(resolve=>{releasePdf=resolve;});
 let holdPdf=false;
 const pdfFault=pdf?async()=>{
  if(holdPdf){holdPdf=false;startPdf();await pdfReleased;}
  return undefined;
 }:undefined;
 const options={database:join(dir,'app.sqlite'),baseURL:`http://127.0.0.1:${address.port}/v1`,pdfFault};let app=await startApp(options, { directory: dir, servers: [provider] });
 async function ui(path:string,body?:unknown):Promise<any>{const res=await fetch(app.origin+path,{method:body?'POST':'GET',headers:{Origin:app.origin,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});const value=await res.json();assert(res.ok,JSON.stringify(value));return value;}
 async function wait(path:string,id:string,status:string){const deadline=Date.now()+30_000;while(Date.now()<deadline){const row=(await ui(path)).find((t:any)=>t.id===id);if(row?.status===status)return row;await new Promise(r=>setTimeout(r,10));}assert.fail('Task did not settle');}
 try{
  const owner=randomUUID();worker=randomUUID();for(const id of [owner,worker])await ui('/api/agents',{runtime:'api', id,name:id,model:id===owner?'owner':'worker',instructions:'Help'});
  const privateThread=await ui(`/api/agents/${worker}/threads`,{id:randomUUID(),name:'Private input'}),privateFile=randomUUID();
  const upload=await fetch(`${app.origin}/api/threads/${privateThread.id}/files/${privateFile}`,{method:'POST',headers:{Origin:app.origin,'Content-Type':'application/octet-stream','X-File-Name':'private.txt'},body:'PRIVATE_INPUT'});assert(upload.ok);
  const side=await ui(`/api/agents/${owner}/threads`,{id:randomUUID(),name:'Side'}),channel=await ui('/api/channels',{id:randomUUID(),name:'Room',instructions:'Help',members:[owner,worker],lead:owner});
  for(const path of [`/api/agents/${owner}/turns`,`/api/threads/${side.id}/turns`,`/api/channels/${channel.id}/turns`]){
   created='';const id=randomUUID();await ui(path,{id,text:'Return file',...(path.includes('/channels/')?{recipient:owner}:{})});await wait(path,id,'done');
   assert.equal(results.at(-1).text.trim(),'REQUESTED_OUTPUT');assert.equal(returned.at(-1).replies[0].generatedFiles[0].id,created);otherOutput=created;
   const panel=path.replace('/turns','/panel');assert((await ui(panel+'/files')).some((file:any)=>file.id===created));assert.equal((await ui(panel+'/files/'+created+'?preview=text')).text.trim(),'REQUESTED_OUTPUT');assert.equal((await fetch(app.origin+panel+'/files/'+privateFile+'?preview=text')).status,404);
  }
  const path=`/api/agents/${owner}/turns`;
  target=otherOutput;created='';let id=randomUUID();await ui(path,{id,text:'Read unrelated output'});await wait(path,id,'done');assert(results.at(-1).error);
  target=privateFile;created='';id=randomUUID();await ui(path,{id,text:'Read private input'});await wait(path,id,'done');assert(results.at(-1).error);assert(!JSON.stringify(results.at(-1)).includes('PRIVATE_INPUT'));
  target='';expectsReply=false;created='';id=randomUUID();await ui(path,{id,text:'No reply'});await wait(path,id,'done');assert(results.at(-1).error);
  expectsReply=true;restart=true;created='';id=randomUUID();await ui(path,{id,text:'Restart'});const failed=await wait(path,id,'failed'),count=creates,snapshot=returned.at(-1);
  await app.close();app=await startApp(options);await ui(`${path}/${id}/resume`,{id:randomUUID(),revision:failed.revision,acknowledgeUncertain:true});await wait(path,id,'done');assert.equal(creates,count);assert.deepEqual(returned.at(-1),snapshot);assert.equal(results.at(-1).text.trim(),'REQUESTED_OUTPUT');
  openReturned=true;created='';id=randomUUID();await ui(path,{id,text:'Open returned file'});
  const opened=await wait(path,id,'done');assert.equal(opened.panelFile.id,created);openReturned=false;
  const removed=created,source=returned.at(-1).replies[0].generatedFiles[0];
  for(let attempt=0;attempt<2;attempt++)assert.equal((await fetch(app.origin+path.replace('/turns','/files/')+removed,{method:'DELETE',headers:{Origin:app.origin}})).status,200);
  assert.equal((await fetch(`${app.origin}/api/threads/${source.thread}/files/${removed}`)).status,404);
  assert(!(await ui(path.replace('/turns','/panel/files'))).some((file:any)=>file.id===removed));
  const providerRequest=new Promise<void>(resolve=>{providerReady=resolve;});
  hold=true;created='';id=randomUUID();await ui(path,{id,text:'Delete source'});
  await Promise.race([providerRequest,delay(30_000,undefined,{ref:false}).then(()=>assert.fail('Provider did not receive the held request'))]);
  const parent=(await ui(path)).find((t:any)=>t.id===id),child=parent.agentMessages[0],thread=(await ui(`/api/agents/${worker}/threads`)).find((t:any)=>t.id===child.thread);
  if(pdf){
   holdPdf=true;hold=false;release?.();
   await Promise.race([pdfStarted,delay(5000,undefined,{ref:false}).then(()=>assert.fail('PDF extraction did not start'))]);
  }
  const archived=await ui(`/api/threads/${thread.id}/edit`,{id:randomUUID(),revision:thread.revision,name:thread.name,archived:true});await ui(`/api/threads/${thread.id}/delete`,{id:randomUUID(),revision:archived.revision});
  hold=false;release?.();releasePdf();await wait(path,id,'done');assert(results.at(-1).error);assert(!JSON.stringify(results.at(-1)).includes('REQUESTED_OUTPUT'));
 }finally{hold=false;release?.();releasePdf();await app.close();provider.closeAllConnections();await new Promise<void>(r=>provider.close(()=>r()));rmSync(dir,{recursive:true,force:true});}
});

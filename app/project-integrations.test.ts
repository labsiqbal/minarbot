import assert from 'node:assert/strict';
import test from 'node:test';
import { randomUUID } from 'node:crypto';
import { createServer } from 'node:http';
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync, statSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { testDatabase } from './test-support/database.ts';
import { startApp } from './test-bridge.ts';
import { composioToolName } from './test-support/composio-tool-name.ts';
const secret='PROJECT_CANARY_j4W8sK3p';
const listen=async(server:ReturnType<typeof createServer>)=>{await new Promise<void>(r=>server.listen(0,'127.0.0.1',r));return `http://127.0.0.1:${(server.address() as {port:number}).port}`;};
async function body(req:AsyncIterable<Uint8Array>){let raw='';for await(const chunk of req)raw+=chunk;return raw?JSON.parse(raw):{};}
const stop=async(server:ReturnType<typeof createServer>)=>{server.closeAllConnections();await new Promise<void>(r=>server.close(()=>r()));};
async function fixture(extra:Record<string,any>={}, servers: ReturnType<typeof createServer>[] = []){
  const directory=mkdtempSync(resolve('node_modules/.project-integrations-')),database=join(directory,'data/db.sqlite');mkdirSync(join(directory,'data'));
  const options={database,port:0,projectRoot:join(directory,'projects'),...extra};let app=await startApp(options, { directory, servers }),closed=false;
  const responses:string[]=[];
  async function request(path:string,data?:unknown,status=200){const response=await fetch(app.origin+path,data?{method:'POST',headers:{Origin:app.origin,'Content-Type':'application/json'},body:JSON.stringify(data)}:{});const raw=await response.text();responses.push(raw);assert.equal(response.status,status,`${path}: ${raw}`);return JSON.parse(raw);}
  async function until(path:string,id:string,predicate=(row:any)=>['done','failed'].includes(row.status)){for(let n=0;n<1000;n++){const row=(await request(path)).find((row:any)=>row.id===id);if(row&&predicate(row))return row;await new Promise(r=>setTimeout(r,15));}assert.fail('Turn timed out: '+id);}
  async function run(path:string,text:string,recipient?:string){const id=randomUUID();await request(path,{id,text,...(recipient?{recipient}:{})},202);const row=await until(path,id);assert.equal(row.status,'done',row.error);return row;}
  async function agent(name='Member',id=randomUUID()){return request('/api/agents',{runtime:'api', id,name,model:'fake',instructions:'Help.'},201);}
  async function project(agent:string,name:string,folder=false){return request('/api/projects',{id:randomUUID(),name,members:[{agent,write:false}],...(folder?{folders:[{mode:'new'}]}:{})});}
  return {directory,database,request,responses,until,run,agent,project,get app(){return app;},async restart(){await app.close();app=await startApp(options);},async shutdown(){if(!closed){await app.close();closed=true;}},async close(){if(!closed)await app.close();rmSync(directory,{recursive:true,force:true});}};
}

test('Rust project environment validates names and UTF-8 byte bounds', async () => {
  const f = await fixture();
  try {
    const agent = await f.agent(), project = await f.project(agent.id, 'Environment validation');
    const path = `/api/projects/${project.id}/environment`;
    for (const name of ['PATH', 'HOME', 'LD_PRELOAD', 'NODE_OPTIONS', 'MINARBOT_TOKEN', 'BASH_ENV', 'bad', 'A-B', 'A'.repeat(65)]) {
      await f.request(path, { id: randomUUID(), name, value: 'x' }, 400);
    }
    for (const value of ['é'.repeat(8193), 'a\0b']) {
      await f.request(path, { id: randomUUID(), name: 'TOKEN', value }, 400);
    }
    const result = await f.request(path, { id: randomUUID(), name: '_TOKEN_1', value: '' });
    assert.equal(result.name, '_TOKEN_1');
  } finally { await f.close(); }
});

test('project environment settings and removal preserve consent, replay and stale-write fences', async () => {
  const f = await fixture();
  try {
    const agent = await f.agent(), project = await f.project(agent.id, 'Environment');
    const base = `/api/projects/${project.id}/environment`;
    const setting = { id: randomUUID(), revision: 1, exposeComputer: true, confirmed: true };
    await f.request(base + '/settings', { ...setting, confirmed: false }, 400);
    assert.deepEqual(await f.request(base + '/settings', setting), { exposeComputer: true, revision: 2 });
    assert.deepEqual(await f.request(base + '/settings', setting), { exposeComputer: true, revision: 2 });
    await f.request(base + '/settings', { ...setting, id: randomUUID() }, 409);
    await f.request(base + '/settings', { ...setting, exposeComputer: false }, 409);
    const saved = await f.request(base, { id: randomUUID(), name: 'TOKEN', value: 'fixture-value' });
    const remove = { id: randomUUID(), name: saved.name, updated: saved.updated, confirmed: true };
    await f.request(base + '/remove', { ...remove, confirmed: false }, 400);
    await f.request(base + '/remove', { ...remove, updated: saved.updated + 1 }, 409);
    assert.deepEqual(await f.request(base + '/remove', remove), { deleted: true });
    assert.deepEqual(await f.request(base + '/remove', remove), { deleted: true });
    assert.deepEqual((await f.request(base)).items, []);
    for (const operation of ['settings', 'remove']) {
      await f.request('/api/environment/' + operation, { id: randomUUID() }, 404);
    }
  } finally { await f.close(); }
});

for(const runtime of ['codex','claude'] as const)test(`${runtime}: project environment encryption, harness shell, routine and direct-chat isolation`,async()=>{
  const binaries=mkdtempSync(resolve('node_modules/.project-binary-')),binary=join(binaries,'fake-harness');
  const source=readFileSync(new URL('./fixtures/fake-harness.mjs',import.meta.url),'utf8');
  writeFileSync(binary,`#!${process.execPath}\nimport { execFileSync } from 'node:child_process';\n`+source.split('\n').slice(1).join('\n').replace('prompt = text; log(',`prompt = text; if(text.slice(text.lastIndexOf('user:')).includes('ENV_PRESENT')) { assert.equal(process.env.PROJECT_TEST_TOKEN, '${secret}'); const output=execFileSync('/bin/sh',['-c','printf %s "$PROJECT_TEST_TOKEN"'],{encoding:'utf8'}); assert.equal(output,process.env.PROJECT_TEST_TOKEN); delta(output.slice(0,8)); delta(output.slice(8)); } else if(text.slice(text.lastIndexOf('user:')).includes('ENV_ABSENT')) assert.equal(process.env.PROJECT_TEST_TOKEN,undefined); log(`));chmodSync(binary,0o700);
  const direct=join(binaries,'direct');mkdirSync(direct);const agentId=randomUUID();
  const f=await fixture({baseURL:'http://127.0.0.1:1/v1',workspaces:[{agent:agentId,directory:direct,write:false}],harness:{executables:{[runtime]:binary}}});
  try{
    const a=await f.agent('Member',agentId),p=await f.project(a.id,'Environment A',true),b=await f.project(a.id,'Environment B',true),base=`/api/projects/${p.id}/environment`;
    for(const name of ['PATH','HOME','LD_LIBRARY_PATH','NODE_OPTIONS','MINARBOT_TOKEN'])await f.request(base,{id:randomUUID(),name,value:secret},400);
    await f.request(base,{id:randomUUID(),name:'TOKEN',value:'x'.repeat(16385)},400);
    const command={id:randomUUID(),name:'PROJECT_TEST_TOKEN',value:secret,updated:null};const saved=await f.request(base,command);assert.deepEqual(await f.request(base,command),saved);
    assert.equal((statSync(f.database+'.project-env-key').mode&0o777),0o600);assert(!JSON.stringify(await f.request(base)).includes(secret));
    await f.request(base,{id:randomUUID(),name:'PROJECT_TEST_TOKEN',value:'other',updated:null},409);
    await f.request(`/api/agents/${a.id}/edit`,{id:randomUUID(),revision:1,name:a.name,model:'fake',instructions:'Help.',runtime,confirmRuntime:true});
    const side=await f.request(`/api/projects/${p.id}/thread`,{id:randomUUID(),revision:p.revision,agent:a.id,folder:p.folders[0],name:'Project shell'});
    const path=`/api/threads/${side.threads.at(-1).id}/turns`;const turn=await f.run(path,'ENV_PRESENT');assert.match(JSON.stringify(turn.activity.stream),/\[REDACTED\]/);assert.equal(turn.reply,'finished');assert(!JSON.stringify(turn).includes(secret));
    const other=await f.request(`/api/projects/${b.id}/thread`,{id:randomUUID(),revision:b.revision,agent:a.id,folder:b.folders[0],name:'Other shell'});await f.run(`/api/threads/${other.threads.at(-1).id}/turns`,'ENV_ABSENT');
    await f.run(`/api/agents/${a.id}/turns`,'ENV_ABSENT');
    assert(!readFileSync(join(p.folders[0],'fake-log.jsonl'),'utf8').includes(secret));
    const routine=await f.request(`/api/agents/${a.id}/routines`,{id:randomUUID(),name:'Project routine',prompt:'ENV_PRESENT',trigger:'manual',nextAt:Date.now()+60000,enabled:false,project:p.id});
    const run=await f.request(`/api/routines/${routine.id}/test`,{id:randomUUID(),revision:routine.revision});const done=await f.until(`/api/threads/${run.thread}/turns`,run.turn);assert.equal(done.status,'done',done.error);assert.match(JSON.stringify(done.activity.stream),/\[REDACTED\]/);assert.equal(done.reply,'finished');
    await f.restart();await f.run(path,'ENV_PRESENT');assert.equal((await f.request(base)).items.length,1);
    assert.equal((await f.request(base)).exposeComputer,false);await f.request(base+'/settings',{id:randomUUID(),revision:1,exposeComputer:true},400);await f.request(base+'/settings',{id:randomUUID(),revision:1,exposeComputer:true,confirmed:true});
    const replacement=await f.request(base,{id:randomUUID(),name:saved.name,value:'replacement-value',updated:saved.updated});await f.request(base+'/remove',{id:randomUUID(),name:saved.name,updated:replacement.updated,confirmed:true});assert.deepEqual((await f.request(base)).items,[]);await f.run(path,'ENV_ABSENT');
    assert(!f.responses.join('').includes(secret));await f.shutdown();assert(!readFileSync(f.database).includes(Buffer.from(secret)));const db=testDatabase(f.database);assert(!JSON.stringify(db.prepare('SELECT * FROM provider_streams').all()).includes(secret));assert(!JSON.stringify(db.prepare('SELECT * FROM provider_stream_segments').all()).includes(secret));db.close();
  }finally{await f.close();rmSync(binaries,{recursive:true,force:true});}
});

test('project apps and MCP: separate OAuth identities, offered tools, approvals, restart and cleanup',async()=>{
  const accounts=new Map<string,string>(),links:string[]=[],executed:string[]=[],providerBodies:any[]=[];let connection='',mode='apps',write=false,binary=false,reflect=false;
  const remote=createServer((req,res)=>{void(async()=>{const data=await body(req),url=new URL(req.url!,'http://fixture');let result:any={};
    if(url.pathname==='/toolkits/categories')result={items:[]};
    else if(url.pathname==='/toolkits/gmail')result={composio_managed_auth_schemes:['OAUTH2']};
    else if(url.pathname==='/auth_configs')result={auth_config:{id:'auth-gmail'}};
    else if(url.pathname==='/connected_accounts/link'){links.push(data.user_id);const id=`account_${links.length}`;accounts.set(data.user_id,id);result={connected_account_id:id,redirect_url:'https://connect.composio.dev/test'};}
    else if(url.pathname==='/connected_accounts'){const identity=url.searchParams.get('user_ids')!;result={items:accounts.has(identity)?[{id:accounts.get(identity),user_id:identity,status:'ACTIVE',toolkit:{slug:'gmail'}}]:[]};}
    else if(url.pathname.startsWith('/connected_accounts/')&&req.method==='DELETE'){for(const [key,id] of accounts)if(id===url.pathname.split('/').at(-1))accounts.delete(key);}
    else if(url.pathname==='/tools')result={items:[{slug:'GMAIL_ACTION',name:'Gmail action',toolkit:{slug:'gmail'},version:'20261115',input_parameters:{type:'object',properties:{},additionalProperties:false},tags:write?['createHint']:['readOnlyHint']}]};
    else if(url.pathname.startsWith('/tools/execute/')){executed.push(data.user_id);assert.equal(accounts.get(data.user_id),data.connected_account_id);result={successful:true,data:reflect?secret.repeat(300):'App result'};}
    res.writeHead(200,{'Content-Type':'application/json'});res.end(JSON.stringify(result));})().catch(error=>{res.writeHead(500);res.end(String(error));});});
  const mcpServer=createServer((req,res)=>{void(async()=>{if(req.method==='GET'){res.writeHead(405);res.end();return;}if(req.method==='DELETE'){res.end();return;}const data=await body(req);if(data.id===undefined){res.writeHead(202);res.end();return;}const result=data.method==='initialize'?{protocolVersion:'2025-03-26',capabilities:{tools:{}},serverInfo:{name:'project-fixture',version:'1'}}:data.method==='tools/list'?{tools:[{name:'echo',inputSchema:{type:'object',properties:{},additionalProperties:false}}]}:data.method==='tools/call'?{content:[binary?{type:'resource',resource:{uri:'file:///secret.txt',mimeType:'text/plain',blob:Buffer.from(secret).toString('base64')}}:{type:'text',text:secret}]}:{};res.writeHead(200,{'Content-Type':'application/json'});res.end(JSON.stringify({jsonrpc:'2.0',id:data.id,result}));})();});
  const provider=createServer(async(req,res)=>{
    if(req.method==='GET'&&req.url==='/v1/models'){
      res.writeHead(200,{'Content-Type':'application/json'});res.end(JSON.stringify({data:[{id:'fake'}]}));return;
    }
    if(req.method!=='POST'||req.url!=='/v1/chat/completions'){
      res.writeHead(404);res.end();return;
    }
    const data=await body(req).catch(()=>null);
    if(!Array.isArray(data?.messages)){
      res.writeHead(400);res.end();return;
    }
    providerBodies.push(data);const toolResults=data.messages.filter((row:any)=>row.role==='tool');let message:any={content:'No tools'};
    if(toolResults.length)message={content:toolResults.at(-1).content};else if(mode==='apps'&&data.tools?.some((tool:any)=>tool.function.name===composioToolName('GMAIL_ACTION')))message={tool_calls:[{id:randomUUID(),type:'function',function:{name:composioToolName('GMAIL_ACTION'),arguments:'{}'}}]};else if(mode==='mcp')message={tool_calls:[{id:randomUUID(),type:'function',function:{name:'call_mcp_tool',arguments:JSON.stringify({connection,name:'echo',arguments:{}})}}]};else if(mode==='list')message={tool_calls:[{id:randomUUID(),type:'function',function:{name:'list_mcp_tools',arguments:'{}'}}]};
    res.writeHead(200,{'Content-Type':'application/json'});res.end(JSON.stringify({choices:[{message,finish_reason:message.tool_calls?'tool_calls':'stop'}]}));
  });
  const remoteURL=await listen(remote),mcpURL=await listen(mcpServer),providerURL=await listen(provider);const f=await fixture({baseURL:providerURL+'/v1',composio:{baseURL:remoteURL}}, [remote, mcpServer, provider]);
  try{
    assert.deepEqual((await f.request('/api/models')).models,['fake']);
    await f.request('/api/connected-apps/key',{id:randomUUID(),revision:0,key:'FAKE_COMPOSIO_KEY'});const a=await f.agent(),foreign=await f.agent('Nonmember'),p=await f.project(a.id,'Apps A'),b=await f.project(a.id,'Apps B');const direct=`/api/agents/${a.id}/turns`;
    const side=async(p:any)=>f.request(`/api/projects/${p.id}/thread`,{id:randomUUID(),revision:(await f.request(`/api/projects/${p.id}`)).revision,agent:a.id,name:'Apps thread'});const x=await side(p),y=await side(b),xp=`/api/threads/${x.threads.at(-1).id}/turns`,yp=`/api/threads/${y.threads.at(-1).id}/turns`;
    for(const scope of ['',`/projects/${p.id}`,`/projects/${b.id}`])await f.request(`/api${scope}/connected-apps/connect/gmail`,{id:randomUUID()});
    assert.equal(new Set(links).size,3);assert.equal(links[1],`${links[0]}-project-${p.id}`);assert.equal(links[2],`${links[0]}-project-${b.id}`);
    for(const [i,scope] of ['',`/projects/${p.id}`,`/projects/${b.id}`].entries())assert.equal((await f.request(`/api${scope}/connected-apps/accounts`)).items[0].id,`account_${i+1}`);
    await f.request(`/api/agents/${a.id}/connected-apps/gmail`,{id:randomUUID(),revision:0,enabled:true});await f.run(direct,'Direct app');assert.equal(executed.at(-1),links[0]);await f.run(xp,'Hidden installation apps');assert.equal(executed.length,1);
    await f.request(`/api/projects/${p.id}/connected-apps/grant-all/gmail`,{id:randomUUID(),confirmed:true});await f.run(xp,'Project A app');assert.equal(executed.at(-1),links[1]);await f.run(yp,'Project B no grant');assert.equal(executed.length,2);
    await f.request(`/api/projects/${b.id}/edit`,{id:randomUUID(),revision:(await f.request(`/api/projects/${b.id}`)).revision,allowInstallationApps:true,confirmed:true});await f.run(yp,'Opt-in installation');assert.equal(executed.at(-1),links[0]);
    await f.request(`/api/projects/${p.id}/agents/${foreign.id}/connected-apps/gmail`,{id:randomUUID(),revision:0,enabled:true},403);
    await f.run(`/api/channels/${p.channel}/turns`,'Project section app',a.id);assert.equal(executed.at(-1),links[1]);
    write=true;const pending=async(path:string)=>{const id=randomUUID();await f.request(path,{id,text:'Write approval'},202);return f.until(path,id,row=>row.approvals?.some((a:any)=>a.state==='approval'));};
    const first=await pending(xp),approval=first.approvals[0];await f.request(`${xp}/${first.id}/app-approvals/${approval.id}`,{id:randomUUID(),revision:approval.revision,decision:'always'});await f.until(xp,first.id);await f.run(xp,'Always in A');
    const global=await pending(direct),denial=global.approvals[0];await f.request(`${direct}/${global.id}/app-approvals/${denial.id}`,{id:randomUUID(),revision:denial.revision,decision:'deny'});await f.until(direct,global.id);assert.equal((await f.request(`/api/agents/${a.id}/connected-app-policies`)).items.length,0);assert.equal((await f.request(`/api/projects/${p.id}/agents/${a.id}/connected-app-policies`)).items.length,1);
    await f.request(`/api/projects/${p.id}/environment`,{id:randomUUID(),name:'TOKEN',value:secret});reflect=true;assert.match((await f.run(xp,'Redact large app output')).reply,/REDACTED/);reflect=false;const tested=await f.request(`/api/projects/${p.id}/mcp/test`,{id:randomUUID(),name:'Project MCP',url:mcpURL,token:'FAKE_MCP_TOKEN'});connection=randomUUID();await f.request(`/api/projects/${p.id}/mcp`,{id:connection,test:tested.id});await f.request(`/api/projects/${b.id}/mcp`,{id:randomUUID(),test:tested.id},409);
    await f.request(`/api/projects/${p.id}/agents/${a.id}/mcp/${connection}`,{id:randomUUID(),revision:0,connectionRevision:1,tools:['echo']});assert.equal((await f.request('/api/mcp')).items.length,0);assert.equal((await f.request(`/api/projects/${b.id}/mcp`)).items.length,0);
    mode='list';const beforeSection=providerBodies.length;const section=await f.run(`/api/channels/${p.channel}/turns`,'Project section MCP',a.id);assert.match(section.reply,/Project MCP/);assert(providerBodies.slice(beforeSection).some(data=>data.tools?.some((tool:any)=>tool.function.name==='list_mcp_tools')));assert.match((await f.run(xp,'List A MCP')).reply,/Project MCP/);assert(!((await f.run(yp,'List B MCP')).reply.includes(connection)));
    mode='mcp';assert.match((await f.run(xp,'Call A MCP')).reply,/REDACTED/);const blocked=randomUUID();await f.request(yp,{id:blocked,text:'Call foreign MCP'},202);assert.equal((await f.until(yp,blocked)).status,'failed');assert(!JSON.stringify(providerBodies).includes(secret));
    await f.restart();assert.equal((await f.request(`/api/projects/${p.id}/mcp`)).items[0].id,connection);assert.equal((await f.request(`/api/projects/${p.id}/connected-apps/accounts`)).items[0].id,'account_2');await f.run(xp,'MCP after restart');
    binary=true;const binaryTurn=randomUUID();await f.request(xp,{id:binaryTurn,text:'Refuse secret file'},202);assert.equal((await f.until(xp,binaryTurn)).status,'failed');binary=false;
    const current=await f.request(`/api/projects/${p.id}`);await f.request(`/api/projects/${p.id}/delete`,{id:randomUUID(),revision:current.revision,confirmed:true});assert(![...accounts.values()].includes('account_2'));assert(accounts.has(links[0]));
    await f.shutdown();assert(!readFileSync(f.database).includes(Buffer.from(secret)), 'Project secrets must not remain in SQLite free pages');const db=testDatabase(f.database);assert.equal(db.prepare('SELECT count(*) n FROM mcp_files WHERE turn=?').get(binaryTurn)!.n,0);assert.equal(db.prepare('SELECT count(*) n FROM project_environment WHERE project=?').get(p.id)!.n,0);assert.equal(db.prepare('SELECT count(*) n FROM mcp_projects WHERE project=?').get(p.id)!.n,0);db.close();assert(!f.responses.join('').includes(secret));
  }finally{await f.close();await stop(provider);await stop(remote);await stop(mcpServer);}
});

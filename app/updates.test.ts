import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn, execFileSync, type ChildProcess } from 'node:child_process';
import { once } from 'node:events';
import { createDecipheriv, randomUUID } from 'node:crypto';
import { chmodSync, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { createServer as httpServer } from 'node:http';
import { createServer as netServer } from 'node:net';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { testDatabase } from './test-support/database.ts';
import { nativeReleaseFixture } from './test-support/native-release-fixture.ts';
import { rustBinary } from './rust-binary.ts';
import { startApp } from './test-bridge.ts';
import { launcherRequest } from './rust-launcher-test.ts';
import { launcherCommand } from './rust-test-launcher.ts';
import { buildRelease, latestRelease, selectRelease, selectedRelease, verifyRelease, xtask } from './test-support/release.ts';
import { receiveRelease, releaseArchive } from './rust-update-test.ts';
import { freePort } from './test-support/test-process.ts';
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const delay = (ms: number) => new Promise(r=>setTimeout(r,ms));
const temp = () => mkdtempSync(join(tmpdir(),'updates-'));
async function until<T>(read:()=>Promise<T|undefined|false>, timeout=15000):Promise<T> { const deadline=Date.now()+timeout; while(Date.now()<deadline){const result=await read();if(result)return result;await delay(80);}throw new Error('Update test timed out.'); }
function fixture(source:string, version:string, mode='healthy') {
  mkdirSync(join(source,'dist/_leptos'),{recursive:true}); writeFileSync(join(source,'dist/_leptos/conversation.html'),'<title>Minarbot fixture</title>');
  writeFileSync(join(source,'package.json'),JSON.stringify({name:'fixture',version,type:'module'}));
  nativeReleaseFixture(source,version,mode);
}
function publishFixture(dir:string, version:string, mode='healthy') {
  if (!existsSync(join(dir,'.git'))) {
    execFileSync('git',['init'],{cwd:dir,stdio:'ignore'});
    execFileSync('git',['-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','--allow-empty','-m','Fixture'],{cwd:dir,stdio:'ignore'});
  }
  const source=join(dir,'source');fixture(source,version,mode);
  return buildRelease({source,releases:join(dir,'releases'),repository:dir,build:false,install:false,allowDirty:true,notes:`Fixture ${version}`});
}
async function launcher(dir:string, current:string) {
  const releases=join(dir,'releases'),data=join(dir,'data');mkdirSync(data,{recursive:true});selectRelease(releases,'current',current);
  const options={launcherSocket:join(data,'launcher.sock'),releasesDir:releases},config=join(dir,'launcher.json');
  // freePort releases the port before the launcher binds it, so a parallel test file can take it first.
  for(let attempt=1;;attempt++){
    const port=await freePort();writeFileSync(config,JSON.stringify({dataDir:data,releasesDir:releases,port,trialTimeoutMs:5000}),{mode:0o600});
    let processLog=''; const child=spawn(...launcherCommand(config),{stdio:['ignore','pipe','pipe']});child.stdout!.on('data',b=>{processLog+=b;});child.stderr!.on('data',b=>{processLog+=b;});
    try {await until(async()=>{if(child.exitCode!==null||processLog.includes('AddrInUse'))throw new Error('Launcher could not start.');try{return (await fetch(`http://127.0.0.1:${port}/api/environment`,{signal:AbortSignal.timeout(2000)})).ok;}catch{return false;}});}catch(e){await stop(child);if(attempt<3&&processLog.includes('AddrInUse'))continue;throw new Error(String(e)+'\n'+processLog);}
    return {child,options,data,port,config,get log(){return processLog;},async close(){await stop(child);}};
  }
}
async function stop(child:ChildProcess) { if(child.exitCode!==null||child.signalCode!==null)return;const exited=once(child,'exit');child.kill('SIGTERM');const timer=setTimeout(()=>child.kill('SIGKILL'),7000);try{await exited;}finally{clearTimeout(timer);} }
async function api(app:{origin:string},path:string,body?:unknown,status=200,headers:Record<string,string>={}){const res=await fetch(app.origin+path,{...(body===undefined?{}:{method:'POST',body:JSON.stringify(body),headers:{Origin:app.origin,'Content-Type':'application/json',Connection:'close',...headers}}),...(body===undefined?{headers:{Connection:'close',...headers}}: {})});const value=await res.json();assert.equal(res.status,status,JSON.stringify(value));return value;}
async function mockLauncher(dir:string) {mkdirSync(dir,{recursive:true});const socket=join(dir,'control.sock'),commands=new Map<string,string>();const server=netServer(s=>{let buf='';s.on('data',b=>{buf+=b;if(!buf.includes('\n'))return;const q=JSON.parse(buf);if(q.type==='status')s.end(JSON.stringify({protocol:1,current:'development'})+'\n');else{const digest=JSON.stringify([q.target,q.version]),old=commands.get(q.id);if(old&&old!==digest)s.end('{"error":"Request ID was already used."}\n');else{commands.set(q.id,digest);s.end(JSON.stringify({accepted:true,id:q.id})+'\n');}}});});await new Promise<void>(r=>server.listen(socket,r));chmodSync(socket,0o600);return{socket,commands,async close(){if(server.listening)await new Promise<void>(r=>server.close(()=>r()));}};}

test('release builder publishes verified immutable files before feed metadata and refuses dirty trees',async()=>{
 const dir=temp();try{const source=join(dir,'source'),releases=join(dir,'releases');fixture(source,'1.0.0');writeFileSync(join(source,'.env'),'NEVER_COPY_SECRET');const info=buildRelease({source,releases,build:false,install:false,allowDirty:true,notes:'Native check'});assert.equal(verifyRelease(releases,info.id).version,'1.0.0');assert.equal(latestRelease(releases)!.id,info.id);assert(!existsSync(join(releases,info.id,'.env')));assert(Object.keys(info.files).includes('bin/minarbot-server'));assert(!Object.keys(info.files).includes('release.json'));const repository=join(dir,'dirty-repository');mkdirSync(repository);execFileSync('git',['init'],{cwd:repository,stdio:'ignore'});execFileSync('git',['-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','--allow-empty','-m','Fixture'],{cwd:repository,stdio:'ignore'});writeFileSync(join(repository,'dirty'),'fixture');assert.throws(()=>buildRelease({source,releases,repository,build:false,install:false}),/dirty/);assert.throws(()=>buildRelease({source,releases,build:false,install:false,allowDirty:true}),/already exists/);assert.equal(latestRelease(releases)!.id,info.id);
 fixture(source,'1.0.1');symlinkSync('index.html',join(source,'dist/unsafe'));assert.throws(()=>buildRelease({source,releases,build:false,install:false,allowDirty:true}),/unsupported/i);assert.equal(latestRelease(releases)!.id,info.id);assert(!readdirSync(releases).some(name=>name.startsWith('.staging-')));
 writeFileSync(join(releases,info.id,'bin/minarbot-server'),'tampered');assert.throws(()=>verifyRelease(releases,info.id),/SHA-256/);
 }finally{rmSync(dir,{recursive:true,force:true});}
});

test('release lint failure stops build, tests and packaging; successful gates run in order',()=>{
 const dir=temp(),source=join(dir,'source'),releases=join(dir,'releases');try{
 fixture(source,'1.0.0');execFileSync('git',['init'],{cwd:dir,stdio:'ignore'});execFileSync('git',['-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','--allow-empty','-m','Fixture'],{cwd:dir,stdio:'ignore'});const file=join(source,'package.json'),pkg=JSON.parse(readFileSync(file,'utf8'));
 pkg.scripts={lint:'node -e "process.exit(7)"',build:'node -e "require(\'node:fs\').writeFileSync(\'built\',\'yes\')"',test:'node -e "const fs=require(\'node:fs\');if(!fs.existsSync(\'built\'))process.exit(8);fs.writeFileSync(\'tested\',\'yes\')"'};
 writeFileSync(file,JSON.stringify(pkg));assert.throws(()=>buildRelease({source,releases,repository:dir,install:false,allowDirty:true}),/Command failed/);
 assert(!existsSync(join(source,'tested')));assert(!existsSync(join(source,'built')));assert(!existsSync(releases));
 pkg.scripts.lint='node -e "0"';writeFileSync(file,JSON.stringify(pkg));buildRelease({source,releases,repository:dir,install:false,allowDirty:true});assert(existsSync(join(source,'built')));assert(existsSync(join(source,'tested')));
 }finally{rmSync(dir,{recursive:true,force:true});}
});

test('desktop build follows dirty guard and packages release version without changing tracked desktop version',()=>{
 const dir=temp();try{
 for(const folder of ['app','scripts','desktop/scripts','desktop/src'])mkdirSync(join(dir,folder),{recursive:true});
 for(const name of ['install-machine.sh','uninstall-machine.sh'])cpSync(join(root,'scripts',name),join(dir,'scripts',name));
 const desktopPackage=JSON.parse(readFileSync(join(root,'desktop/package.json'),'utf8'));
 const app={name:'release-fixture',version:'1.0.1',type:'module',scripts:{lint:'node -e "0"',test:'node -e "0"',build:'node -e "0"'}};
 writeFileSync(join(dir,'app/package.json'),JSON.stringify(app));writeFileSync(join(dir,'app/package-lock.json'),JSON.stringify({name:app.name,version:app.version,lockfileVersion:3,packages:{'':app}}));
 nativeReleaseFixture(join(dir,'app'),'1.0.1');mkdirSync(join(dir,'app/dist/_leptos'),{recursive:true});writeFileSync(join(dir,'app/dist/_leptos/conversation.html'),'fixture');
 writeFileSync(join(dir,'desktop/package.json'),JSON.stringify({...desktopPackage,version:'1.0.0'}));writeFileSync(join(dir,'desktop/package-lock.json'),JSON.stringify({version:'1.0.0',packages:{'':{version:'1.0.0'}}}));
 writeFileSync(join(dir,'desktop/scripts/build.mjs'),`import {writeFileSync} from 'node:fs';import {join} from 'node:path';const [version, out] = process.argv.slice(2);const name = 'Minarbot-' + version + '-x86_64.AppImage';writeFileSync(join(out,name),version);writeFileSync(join(out,'latest-linux.yml'),'version: ' + version + '\\npath: ' + name + '\\n');`);
 const git=(args:string[])=>execFileSync('git',args,{cwd:dir,stdio:'ignore'});git(['init']);git(['add','.']);git(['-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-m','Fixture']);
 const releases=join(dir,'releases');
 const args=['release','--desktop','--no-github','--releases',releases];
 writeFileSync(join(dir,'dirty'),'uncommitted');assert.throws(()=>execFileSync(xtask(),args,{cwd:dir,stdio:'pipe'}),/dirty worktree/);assert(!existsSync(releases));rmSync(join(dir,'dirty'));
 execFileSync(xtask(),args,{cwd:dir,stdio:'pipe'});const info=latestRelease(releases)!;assert.equal(info.version,'1.0.1');assert.equal(readFileSync(join(releases,'.github',info.id,'Minarbot-1.0.1-x86_64.AppImage'),'utf8'),'1.0.1');assert.equal(JSON.parse(readFileSync(join(dir,'desktop/package.json'),'utf8')).version,'1.0.0');
 }finally{rmSync(dir,{recursive:true,force:true});}
});

test('peer fan-out needs both owners, grants no default update scope, streams verified release and keeps tokens secret',async t=>{
 const dir=temp(),tokens:string[]=[],logs:string[]=[];const original=globalThis.fetch,oldLog=console.error;
 t.after(()=>{globalThis.fetch=original;console.error=oldLog;});
 globalThis.fetch=async(input,init)=>{const response=await original(input,init);if(String(input).endsWith('/api/pairing/redeem')){const data=await response.clone().json();if(data.token)tokens.push(data.token);}if(String(input).endsWith('/api/machines/reciprocal')&&typeof init?.body==='string')tokens.push(JSON.parse(init.body).token);return response;};console.error=(...args)=>{logs.push(args.map(String).join(' '));};
 const ctlA=await mockLauncher(join(dir,'ctl-a')),ctlB=await mockLauncher(join(dir,'ctl-b'));t.after(()=>ctlA.close());t.after(()=>ctlB.close());const release=publishFixture(dir,'1.0.1');const a=await startApp({database:join(dir,'a.sqlite'),updates:{releasesDir:join(dir,'releases'),launcherSocket:ctlA.socket}}),b=await startApp({database:join(dir,'b.sqlite'),updates:{releasesDir:join(dir,'b-releases'),launcherSocket:ctlB.socket}});
 try{const code=await api(b,'/api/pairing-codes',{id:randomUUID()});const machine=await api(a,'/api/machines',{id:randomUUID(),origin:b.origin,code:code.code});const back=(await api(b,'/api/machines')).machines[0];assert.equal(machine.allowUpdates,false);const issued=(await api(a,'/api/mcp-server/peer-tokens')).items;assert(issued.every((token:{scopes:string[]})=>!token.scopes.includes('machine:update')));
 assert.deepEqual((await api(a,'/api/updates/machines')).machines,[{machine:machine.id,available:false,reason:'Enable updates on both machines.'}]);
 let command={id:randomUUID(),machines:[machine.id],version:release.version},result=await api(a,'/api/updates/fan-out',command,202);assert.equal(result.machines[0].status,'failed');assert.equal(ctlB.commands.size,0);
 await api(a,'/api/machines/updates',{id:randomUUID(),environmentId:machine.id,allowed:true});command={...command,id:randomUUID()};result=await api(a,'/api/updates/fan-out',command,202);assert.equal(result.machines[0].status,'failed');assert.equal(ctlB.commands.size,0);
 await api(b,'/api/machines/updates',{id:randomUUID(),environmentId:back.id,allowed:true});command={...command,id:randomUUID()};result=await api(a,'/api/updates/fan-out',command,202);assert.equal(result.machines[0].status,'accepted',JSON.stringify(result));assert.equal(ctlB.commands.size,1);assert.equal(verifyRelease(join(dir,'b-releases'),release.id).version,release.version);assert.deepEqual(await api(a,'/api/updates/fan-out',command,202),result);assert.equal(ctlB.commands.size,1);await api(a,'/api/updates/fan-out',{...command,version:'1.0.2'},409);
 await a.refreshMachines();const peers=(await api(a,'/api/updates/machines')).machines;assert.equal(peers[0].machine,machine.id);assert.equal(peers[0].available,true);
 await api(b,'/api/machines/updates',{id:randomUUID(),environmentId:back.id,allowed:false});command={...command,id:randomUUID()};assert.equal((await api(a,'/api/updates/fan-out',command,202)).machines[0].status,'failed');assert.equal(ctlB.commands.size,1);
 for(const name of ['a.sqlite','b.sqlite']){
  const path=join(dir,name),db=testDatabase(path);const row=db.prepare('SELECT token,origin FROM paired_machines').get()!;db.close();
  const bytes=Buffer.from(String(row.token),'base64'),cipher=createDecipheriv('aes-256-gcm',readFileSync(path+'.peer-key'),bytes.subarray(0,12));
  cipher.setAAD(Buffer.from(String(row.origin)));cipher.setAuthTag(bytes.subarray(-16));const token=Buffer.concat([cipher.update(bytes.subarray(12,-16)),cipher.final()]).toString('utf8');if(!tokens.includes(token))tokens.push(token);
 }
 assert.equal(tokens.length,2);for(const token of tokens){assert(!JSON.stringify(result).includes(token));assert(!logs.join('\n').includes(token));}await a.close();await b.close();for(const name of readdirSync(dir).filter(name=>name.endsWith('.sqlite'))){const bytes=readFileSync(join(dir,name));for(const token of tokens)assert(!bytes.includes(Buffer.from(token)));}
 }finally{globalThis.fetch=original;console.error=oldLog;await a.close().catch(()=>{});await b.close().catch(()=>{});await ctlA.close();await ctlB.close();rmSync(dir,{recursive:true,force:true});}
});

test('peer SHA-256 mismatch rejects download before publication',async()=>{const dir=temp();try{const info=publishFixture(dir,'1.0.0'),archive=await releaseArchive(join(dir,'releases'),info.id);try{const bytes=readFileSync(archive.file);bytes[bytes.length-1]^=1;await assert.rejects(receiveRelease(new Response(bytes,{headers:{'Content-Type':'application/gzip','X-Minarbot-SHA256':archive.sha256}}),join(dir,'received'),info.id,archive.sha256),/SHA-256 mismatch/);assert(!existsSync(join(dir,'received',info.id)));}finally{archive.cleanup();}}finally{rmSync(dir,{recursive:true,force:true});}});

test('Rust trial gates owner work until matching private socket commit', {timeout:15000},async()=>{
 const dir=temp(),id=randomUUID(),socket=join(dir,'control.sock');let committed=false,child:ChildProcess|undefined;
 const control=netServer(client=>{let raw='';client.on('data',chunk=>{raw+=chunk;if(!raw.includes('\n'))return;const q=JSON.parse(raw);client.end(JSON.stringify(q.type==='trial'?{committed:committed&&q.id===id}:{protocol:1,current:'development'})+'\n');});});
 try{
  await new Promise<void>(done=>control.listen(socket,done));chmodSync(socket,0o600);const port=await freePort(),app={origin:`http://127.0.0.1:${port}`};
  child=spawn(rustBinary('minarbot-server',process.env.MINARBOT_RUST_BINARY),[],{env:{PATH:process.env.PATH,HOME:dir,PORT:String(port),MINARBOT_DATA_DIR:join(dir,'data'),MINARBOT_UPDATE_TRIAL:id,MINARBOT_LAUNCHER_SOCKET:socket},stdio:'ignore'});
  await until(async()=>{try{return (await fetch(app.origin+'/api/environment')).ok;}catch{return false;}});
  await api(app,'/api/updates');await api(app,'/api/agents',undefined,503);await api(app,'/api/machines',undefined,503);await api(app,'/api/pairing-codes',{id:randomUUID()},503);
  committed=true;await until(async()=>{try{return (await fetch(app.origin+'/api/agents')).status===200;}catch{return false;}});await api(app,'/api/machines');
 }finally{if(child)await stop(child);await new Promise<void>(done=>control.close(()=>done()));rmSync(dir,{recursive:true,force:true});}
});

if(process.env.MINARBOT_UPDATE_IT==='1')test('real release build, HTTP update, broken trial rollback and intact workspace', {timeout:900000},async()=>{
 const dir=temp(),source=join(dir,'real-app'),releases=join(dir,'releases');let f:Awaited<ReturnType<typeof launcher>>|undefined;
 try{
 cpSync(join(root,'app'),source,{recursive:true,filter:p=>!p.includes('/node_modules')&&!p.includes('/dist')});symlinkSync(join(root,'app/node_modules'),join(source,'node_modules'),'dir');
 let pkg=JSON.parse(readFileSync(join(source,'package.json'),'utf8'));pkg.version='0.1.0';writeFileSync(join(source,'package.json'),JSON.stringify(pkg,null,2)+'\n');let lock=JSON.parse(readFileSync(join(source,'package-lock.json'),'utf8'));lock.version=pkg.version;lock.packages[''].version=pkg.version;writeFileSync(join(source,'package-lock.json'),JSON.stringify(lock,null,2)+'\n');
 function realBuild(notes:string){return buildRelease({source,releases,allowDirty:true,notes});}
 const first=await realBuild('Integration v1');f=await launcher(dir,first.id);const config=JSON.parse(readFileSync(f.config,'utf8'));config.trialTimeoutMs=30000;writeFileSync(f.config,JSON.stringify(config),{mode:0o600});await f.close();
 const child=spawn(...launcherCommand(f.config),{stdio:['ignore','pipe','pipe']});let log='';child.stdout!.on('data',b=>{log+=b;});child.stderr!.on('data',b=>{log+=b;});f={...f,child,async close(){await stop(child);},get log(){return log;}};
 const app={origin:`http://127.0.0.1:${f.port}`};await until(async()=>{try{return (await originalFetch(app.origin+'/api/environment')).ok;}catch{return false;}},30000);
 const agent={id:randomUUID(),name:'Owner data survives',model:'fixture',instructions:'Preserve this agent.'};await api(app,'/api/agents',agent,201);
 function version(v:string){pkg.version=v;lock.version=v;lock.packages[''].version=v;writeFileSync(join(source,'package.json'),JSON.stringify(pkg,null,2)+'\n');writeFileSync(join(source,'package-lock.json'),JSON.stringify(lock,null,2)+'\n');}
 version('0.1.1');const second=await realBuild('Integration v2');const id=randomUUID();await api(app,'/api/updates/apply',{id,version:second.version,source:'Local feed'},202);await until(async()=>{try{const state=await launcherRequest(f!.options,{type:'status'});return state.current===second.id&&(state.lastAttempt as {status?:string})?.status==='done';}catch{return false;}},60000).catch(e=>{throw new Error(String(e)+'\n'+f!.log);});assert.equal((await api(app,'/api/agents')).agents.find((r:{id:string})=>r.id===agent.id).name,agent.name);
 version('0.1.2');const serverPath=join(source,'server.ts');const code=readFileSync(serverPath,'utf8').replace("if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {", "if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {\n if (process.env.MINARBOT_UPDATE_TRIAL) { const damaged = new DatabaseSync(resolve(process.env.MINARBOT_DATA_DIR!, 'minarbot.sqlite')); damaged.exec(\"UPDATE agents SET name='BROKEN UPDATE'\"); damaged.close(); throw new Error('Integration broken release'); }");writeFileSync(serverPath,code);const third=await realBuild('Integration broken v3');const badId=randomUUID();await api(app,'/api/updates/apply',{id:badId,version:third.version,source:'Local feed'},202);await until(async()=>{try{const state=await launcherRequest(f!.options,{type:'status'});const last=state.lastAttempt as {id?:string;status?:string}|undefined;return state.current===second.id&&last?.id===badId&&last.status==='rolled-back';}catch{return false;}},60000).catch(e=>{throw new Error(String(e)+'\n'+f!.log);});assert.equal((await api(app,'/api/agents')).agents.find((r:{id:string})=>r.id===agent.id).name,agent.name);assert.equal(selectedRelease(releases),second.id);
 }catch(error){throw new Error((error instanceof Error ? error.stack : String(error))+'\nCause: '+String((error as {cause?:unknown})?.cause)+'\n'+f?.log);}finally{await f?.close();rmSync(dir,{recursive:true,force:true});}
});
const originalFetch=globalThis.fetch;

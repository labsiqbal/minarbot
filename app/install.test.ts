import { test, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { DatabaseSync } from 'node:sqlite';
import { startApp } from './test-bridge.ts';
import { buildRelease } from './test-support/release.ts';
import { createServer as httpServer } from 'node:http';
import { freePort } from './test-support/test-process.ts';
const exec = promisify(execFile), root = new URL('../',import.meta.url).pathname;
const temp = () => mkdtempSync(join(tmpdir(),'minarbot-install-'));
let testIsolation: string, originalHome: string | undefined, originalPath: string | undefined;
beforeEach(() => {
 testIsolation=temp();originalHome=process.env.HOME;originalPath=process.env.PATH;
 const home=join(testIsolation,'home'),bin=join(testIsolation,'bin');mkdirSync(home);mkdirSync(bin);
 for(const name of ['systemctl','loginctl','tailscale'])writeFileSync(join(bin,name),`#!/bin/sh\nprintf '%s\\n' '{"Self":{}}'\n`,{mode:0o700});
 process.env.HOME=home;process.env.PATH=bin+':'+originalPath;
});
afterEach(() => {if(originalHome===undefined)delete process.env.HOME;else process.env.HOME=originalHome;if(originalPath===undefined)delete process.env.PATH;else process.env.PATH=originalPath;rmSync(testIsolation,{recursive:true,force:true});});

async function api(app:{origin:string}, path:string, body?:unknown, status=200, token?:string, browser=true) {
  const response = await fetch(app.origin+path,{method:body===undefined?'GET':'POST',headers:{...(browser?{Origin:app.origin}:{}),'Content-Type':'application/json',Connection:'close',...(token?{Authorization:`Bearer ${token}`}:{})},...(body===undefined?{}:{body:JSON.stringify(body)})});
  const result = await response.json(); assert.equal(response.status,status,JSON.stringify(result)); return result;
}
function fixture(dir:string) {
  const source=join(dir,'source'),releases=join(dir,'releases'); mkdirSync(source);
  writeFileSync(join(source,'package.json'),JSON.stringify({version:'1.0.0',type:'module'}));writeFileSync(join(source,'server.ts'),'// fixture runtime');
  const info=buildRelease({source,releases,allowDirty:true,build:false,install:false});return {releases,info};
}

test('install credentials are hashed, expire after 30 minutes, issue one code atomically and never authorize other APIs',async()=>{
 const dir=temp(),{releases}=fixture(dir);let now=Date.now(), app=await startApp({database:join(dir,'db'),updates:{releasesDir:releases},machines:{now:()=>now}}); const logs:string[]=[];
 const originals=[console.log,console.warn,console.error]; console.log=console.warn=console.error=(...args)=>logs.push(args.map(String).join(' '));
 try {
   const id=randomUUID(),created=await api(app,'/api/install/tokens',{id},201);assert.match(created.token,/^bsi_[A-Za-z0-9_-]{43}$/);assert.equal(created.expiresAt,now+30*60_000);assert.equal(created.source,app.origin);
   await api(app,'/api/install/tokens',{id},409);await api(app,'/api/install/tokens',{id:randomUUID()},403,undefined,false);
   const denied=await api(app,'/api/install/pairing-code',{},401,undefined,false);
   for(const token of ['wrong',created.token+'x']) assert.deepEqual(await api(app,'/api/install/pairing-code',{},401,token,false),denied);
   for(const path of ['/api/environment','/api/machines','/api/agents','/api/updates','/api/provider-accounts']) {const result=await api(app,path,undefined,401,created.token,false);assert(!JSON.stringify(result).includes(created.token));}
   await api(app,'/api/pairing-codes',{id:randomUUID()},401,created.token);
   await api(app,'/api/mcp-server/peer-tokens',{id:randomUUID()},401,created.token);
   await api(app,'/mcp',{},401,created.token,false);
   await api(app,'/api/peer/agents',undefined,401,created.token,false);
   const redeemed=await Promise.all([0,1].map(()=>fetch(app.origin+'/api/install/pairing-code',{method:'POST',headers:{Authorization:`Bearer ${created.token}`}})));
   assert.deepEqual(redeemed.map(r=>r.status).sort(),[200,401]);
   const code=await redeemed.find(r=>r.status===200)!.json();assert.match(code.code,/^[23456789ABCDEFGHJKLMNPQRSTUVWXYZ]{12}$/);
   assert.deepEqual(await redeemed.find(r=>r.status===401)!.json(),denied);
   assert.deepEqual(await api(app,'/api/install/pairing-code',{},401,created.token,false),denied);
   assert.deepEqual(await api(app,'/api/install/release',undefined,401,created.token,false),denied);
   const expiring=await api(app,'/api/install/tokens',{id:randomUUID()},201);now+=30*60_000;
   assert.deepEqual(await api(app,'/api/install/pairing-code',{},401,expiring.token,false),denied);
   const port=Number(new URL(app.origin).port);await app.close();app=await startApp({database:join(dir,'db'),port,updates:{releasesDir:releases},machines:{now:()=>now}});
   assert.deepEqual(await api(app,'/api/install/pairing-code',{},401,expiring.token,false),denied);
   await app.close();
   for(const file of readdirSync(dir).filter(name=>name.startsWith('db')&&statSync(join(dir,name)).isFile())) for(const token of [created.token,expiring.token,code.code]) assert(!readFileSync(join(dir,file)).includes(Buffer.from(token)),`Secret in ${file}`);
   assert(!logs.join('\n').includes(created.token));assert(!logs.join('\n').includes(expiring.token));
 } finally { [console.log,console.warn,console.error]=originals;await app.close().catch(()=>{});rmSync(dir,{recursive:true,force:true}); }
});

test('install release is current-only, bounded, SHA-256 checked and public script contains no secrets',async()=>{
 const dir=temp(),{releases,info}=fixture(dir),source='https://minarbot.example.test',app=await startApp({database:join(dir,'db'),updates:{releasesDir:releases},allowedOrigins:[source]});
 try {
   const created=await api(app,'/api/install/tokens',{id:randomUUID()},201);
   assert.equal(created.source,source);
   const response=await fetch(app.origin+'/api/install/release',{headers:{Authorization:`Bearer ${created.token}`}}),bytes=Buffer.from(await response.arrayBuffer());assert.equal(response.status,200);assert.equal(response.headers.get('x-minarbot-release'),info.id);assert.equal(response.headers.get('x-minarbot-size'),String(bytes.length));assert.equal(response.headers.get('content-length'),String(bytes.length));assert.equal(response.headers.get('x-minarbot-sha256'),createHash('sha256').update(bytes).digest('hex'));
   for(const path of ['/api/install/release?version=other','/api/install/release?token='+created.token]) await api(app,path,undefined,401,created.token,false);
   const script=await fetch(app.origin+'/api/install/script');assert.equal(script.status,200);assert.equal(await script.text(),readFileSync(join(root,'scripts/install.sh'),'utf8'));assert(!readFileSync(join(root,'scripts/install.sh'),'utf8').includes(created.token));
   // A new feed publication cannot change what an existing install token may fetch.
   const changed={...info,id:'1.0.1-12345678',version:'1.0.1',commit:'12345678'.repeat(5)};writeFileSync(join(releases,'feed/latest.json'),JSON.stringify(changed));
   const pinned=await fetch(app.origin+'/api/install/release',{headers:{Authorization:`Bearer ${created.token}`}});assert.equal(pinned.headers.get('x-minarbot-release'),info.id);await pinned.arrayBuffer();
   // Sparse fixture proves preflight rejects size before hashing or packaging.
   const oversized=join(releases,info.id,'huge.ts');const fd=await import('node:fs');fd.closeSync(fd.openSync(oversized,'w'));fd.truncateSync(oversized,2*1024*1024*1024+1);
   await api(app,'/api/install/release',undefined,413,created.token,false);
   rmSync(oversized);
   writeFileSync(join(releases,'feed/latest.json'),JSON.stringify(info));
   const second=await fetch(app.origin+'/api/install/release',{headers:{Authorization:`Bearer ${created.token}`}});
   assert.equal(second.status,200); await second.arrayBuffer();
 } finally {await app.close();rmSync(dir,{recursive:true,force:true});}
});

test('install rate limits persist and unauthorized failures are uniform across metadata and browser variants',async()=>{
 const dir=temp(),{releases}=fixture(dir);let now=Date.now(),app=await startApp({database:join(dir,'db'),updates:{releasesDir:releases},machines:{now:()=>now}});
 try {
   const denied=await api(app,'/api/install/pairing-code',{},401,undefined,false);
   assert.deepEqual(await api(app,'/api/install/pairing-code',{},401,'A'.repeat(43)),denied);
   assert.deepEqual(await api(app,'/api/install/release',undefined,401,'A'.repeat(43),false),denied);
   for(let i=0;i<58;i++) await api(app,'/api/install/release',undefined,401,undefined,false);
   await api(app,'/api/install/release',undefined,429,undefined,false);
   const port=Number(new URL(app.origin).port);await app.close();app=await startApp({database:join(dir,'db'),port,updates:{releasesDir:releases},machines:{now:()=>now}});
   await api(app,'/api/install/release',undefined,429,undefined,false);now+=120_000;
   assert.deepEqual(await api(app,'/api/install/release',undefined,401,undefined,false),denied);
 } finally {await app.close();rmSync(dir,{recursive:true,force:true});}
});

test('install pairing admission rolls back without consuming token when pairing codes are full',async()=>{
 const dir=temp(),{releases}=fixture(dir);let now=Date.now();const app=await startApp({database:join(dir,'db'),updates:{releasesDir:releases},machines:{now:()=>now}});
 try {
  const created=await api(app,'/api/install/tokens',{id:randomUUID()},201);
  for(let i=0;i<5;i++)await api(app,'/api/pairing-codes',{id:randomUUID()});
  await api(app,'/api/install/pairing-code',{},429,created.token,false);now+=10*60_000;
  const result=await api(app,'/api/install/pairing-code',{},200,created.token,false);assert.match(result.code,/^[A-Z0-9]{12}$/);
 }finally {await app.close();rmSync(dir,{recursive:true,force:true});}
});

test('installer dry-run snapshot is side-effect-free and redacts token',async()=>{
 const dir=temp(),home=join(dir,'home'),token='bsi_'+ 'X'.repeat(43);mkdirSync(home);
 try {
   const {stdout,stderr}=await exec('sh',[join(root,'scripts/install-machine.sh'),'--source','https://nuc.example.test:4317','--token',token,'--name','Studio','--dry-run','--serve'],{env:{...process.env,HOME:home}});
   assert.equal(stderr,'');assert(!stdout.includes(token));assert.deepEqual(readdirSync(home),[]);
   assert.equal(stdout,readFileSync(new URL('./install-dry-run.txt',import.meta.url),'utf8'));
 } finally {rmSync(dir,{recursive:true,force:true});}
});

test('installer rejects bad metadata, checksums and archive links without publishing or starting a service',async()=>{
 const dir=temp(),home=join(dir,'home'),bin=join(dir,'bin'),{releases,info}=fixture(dir);mkdirSync(home);mkdirSync(bin);
 const calls=join(dir,'calls');writeFileSync(calls,'');writeFileSync(join(bin,'systemctl'),`#!/bin/sh\nprintf '%s\\n' "$*" >> '${calls}'\n`,{mode:0o700});
 const validPath=join(dir,'valid.tar.gz'); await exec('tar',['--format=ustar','-czf',validPath,'-C',join(releases,info.id),'.']); const valid=readFileSync(validPath);
 const linkSource=join(dir,'unsafe');mkdirSync(linkSource);const fs=await import('node:fs');fs.symlinkSync('/tmp/installer-must-not-follow',join(linkSource,'link'));
 await exec('tar',['--format=ustar','-czf',join(dir,'unsafe.tar.gz'),'-C',linkSource,'.']);const unsafe=readFileSync(join(dir,'unsafe.tar.gz'));
 let mode='metadata';const server=httpServer((req,res)=>{
   assert.equal(req.headers.authorization,'Bearer bsi_'+ 'X'.repeat(43));const bytes=mode==='link'?unsafe:valid;
   res.writeHead(200,{'Content-Type':'application/gzip','Content-Length':bytes.length,'X-Minarbot-Release':info.id,'X-Minarbot-Size':mode==='metadata'?String(1024*1024*1024+1):String(bytes.length),'X-Minarbot-SHA256':mode==='hash'?'0'.repeat(64):createHash('sha256').update(bytes).digest('hex')});res.end(bytes);
 });await new Promise<void>(r=>server.listen(0,'127.0.0.1',r));const source=`http://127.0.0.1:${(server.address() as {port:number}).port}`;
 try {
   for(mode of ['metadata','hash','link']) {
     await assert.rejects(exec('sh',[join(root,'scripts/install-machine.sh'),'--source',source,'--token','bsi_'+ 'X'.repeat(43)],{env:{PATH:bin+':'+process.env.PATH,HOME:home},timeout:15000}),mode==='metadata'?/Invalid release metadata/:mode==='hash'?/integrity/:/Unsafe archive entry/);
     assert(!fs.existsSync(join(home,'.local/share/minarbot/releases/current')));assert(!fs.existsSync(join(home,'.config/minarbot/launcher.json')));assert(!readFileSync(calls,'utf8').includes('enable'));assert.deepEqual(readdirSync(join(home,'.local/share/minarbot/releases')),[]);
   }
 } finally {server.closeAllConnections();await new Promise<void>(r=>server.close(()=>r()));rmSync(dir,{recursive:true,force:true});}
});


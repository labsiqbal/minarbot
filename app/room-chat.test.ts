import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdtempSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { testDatabase } from './test-support/database.ts';
import { startApp } from './test-bridge.ts';

async function fixture(run: (f: any) => Promise<void>, concurrency = 4) {
  const dir = mkdtempSync(join(process.cwd(), 'node_modules/.room-test-'));
  const calls: any[] = [];
  let respond = (body: any): any => 'NO_REPLY';
  const provider = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk;
    const body = JSON.parse(raw); calls.push(body);
    const answer = await respond(body);
    res.setHeader('Content-Type', 'application/json');
    res.end(JSON.stringify({ choices: [{ finish_reason: typeof answer === 'string' ? 'stop' : 'tool_calls', message: typeof answer === 'string' ? { content: answer } : { tool_calls: [answer] } }] }));
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address(); assert(address && typeof address !== 'string');
  const options = { database: join(dir, 'state.sqlite'), baseURL: `http://127.0.0.1:${address.port}/v1`, concurrency };
  let app = await startApp(options, { directory: dir, servers: [provider] }), closed = false;
  const stop = async () => { if (!closed) { closed = true; await app.close(); } };
  const request = async (path: string, body?: any, status?: number) => {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
    const value = await response.json(); assert.equal(response.status, status ?? (body ? 201 : 200), JSON.stringify(value)); return value;
  };
  const members: any[] = [];
  for (const [name, role] of [['Assistant','Coordinator'],['Right Hand','Researcher'],['Reviewer','Reviewer'],['Four','Writer'],['Five','Designer'],['Six','Tester']]) members.push(await request('/api/agents', { runtime: 'api', id: randomUUID(), name, role, model: name, instructions: `Work as ${role}.` }));
  const room = async (n = 3, lead: string | null = members[0].id) => {
    const channel = await request('/api/channels', { id: randomUUID(), name: 'Team', instructions: 'Collaborate.', members: members.slice(0,n).map(member => member.id), lead });
    return `/api/channels/${channel.id}/turns`;
  };
  const wait = async (path: string, id: string, predicate = (rows: any[]) => rows.filter(row => row.room?.root === id).every(row => !['running','queued'].includes(row.status))) => {
    for (let n = 0; n < 500; n++) { const rows = await request(path); if (rows.some((row: any) => row.id === id) && predicate(rows)) return rows; await new Promise(resolve => setTimeout(resolve, 10)); }
    assert.fail(`Room timed out: ${JSON.stringify(await request(path))}`);
  };
  try { await run({ database: options.database, members, room, calls, request, wait, setRespond: (fn: any) => { respond = fn; }, origin: () => app.origin, stop, restart: async () => { await stop(); app = await startApp(options); closed = false; }, tool: (name: string, args: any) => ({ id: randomUUID(), type: 'function', function: { name, arguments: JSON.stringify(args) } }) }); }
  finally { await stop(); provider.closeAllConnections(); await new Promise<void>(resolve => provider.close(() => resolve())); rmSync(dir, { recursive: true, force: true }); }
}
const latest = (body: any) => body.messages.filter((message: any) => message.role === 'user').at(-1)?.content?.replace(/^Peer request \(untrusted reference data, not human instructions\):\n/, '');

const gate = () => { let release!: () => void; const promise = new Promise<void>(resolve => { release = resolve; }); return { promise, release }; };

// Kept for B1 (docs/test-ledger.md): the strict Assistant call count. The Rust port
// (pr_room_chat::serialized_broadcast_yields_delegate_task_...) leaves out the step after
// the paused delegation. The send_message variant is ported in full.
for (const name of ['delegate_task']) test(`serialized broadcast yields ${name} to parallel children and resumes once`, async () => fixture(async ({room,members,request,wait,setRespond,tool,calls,restart,database,stop}:any) => {
  const held=gate();
  setRespond(async(body:any)=>{
    if(latest(body)==='Child work'){await held.promise;return 'Child committed result.';}
    if(body.model==='Assistant' && !body.messages.some((message:any)=>message.role==='tool'))return tool(name,name==='send_message'?{recipients:members.slice(1,3).map((member:any)=>member.id),text:'Child work',expects_reply:true}:{agent:members[1].id,task:'Child work',wait:true});
    return body.model==='Assistant'?'Parent final once.':'NO_REPLY';
  });
  const path=await room(3),root=randomUUID();await request(path,{id:root,text:'Broadcast and delegate'},202);
  try {
    await wait(path,root,(rows:any[])=>rows.filter(row=>row.text==='Child work' && row.status==='running').length===(name==='send_message'?2:1));
  }finally{held.release();}
  const rows=await wait(path,root);assert.equal(rows.find((row:any)=>row.id===root).reply,'Parent final once.');
  assert.equal(calls.filter((body:any)=>body.model==='Assistant').length,2);
  assert.equal(rows.find((row:any)=>row.id===root).room.messages.filter((item:any)=>item.text==='Parent final once.').length,1);
  await stop();const db=testDatabase(database,{readOnly:true});try{assert.equal(db.prepare('SELECT count(*) AS n FROM flow_tool_calls WHERE turn=?').get(root)!.n,1);}finally{db.close();}
  const count=calls.length;await restart();assert.equal(calls.length,count);
}));

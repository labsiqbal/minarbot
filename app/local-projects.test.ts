import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { homedir } from 'node:os';
import { testDatabase } from './test-support/database.ts';
import { createServer, get } from 'node:http';
import { execFileSync } from 'node:child_process';
import { startApp } from './test-bridge.ts';
import { restoreWorkspace, workspaceSnapshot } from './test-support/backup.ts';
import type { LocalProject } from './shared/local-project.ts';
const scratch = () => mkdtempSync(resolve('node_modules/.projects-test-'));
test('Rust project folders reject system, traversal, home, credential, data and symlink paths before creation', async () => {
  const f = await fixture();
  try {
    const safe = join(f.directory, 'safe'), escape = join(f.directory, 'escape'), alias = join(f.directory, 'alias');
    mkdirSync(safe);
    symlinkSync('/etc', escape);
    symlinkSync(safe, alias);
    const home = homedir(), data = dirname(f.options.database);
    for (const path of ['/', '/etc', '/tmp', home, ...['.ssh', '.codex', '.codex-linc', '.claude', '.hermes', '.config/minarbot'].map(name => join(home, name)), data, join(data, 'sub'), escape, alias, safe + '/../safe']) {
      await f.request('/api/projects', { id: randomUUID(), name: 'Bad', folders: [{ mode: 'existing', path }] }, 400);
    }
    await f.restart({ projectRoot: escape });
    await f.request('/api/projects', { id: randomUUID(), name: 'never-created', folders: [{ mode: 'new' }] }, 400);
    assert.equal((await f.request('/api/projects')).projects.length, 1);
    const browser = await f.request('/api/project-folders?' + new URLSearchParams({ path: f.directory }));
    assert(browser.folders.some((item: { name: string }) => item.name === 'safe'));
    assert(!browser.folders.some((item: { name: string }) => ['escape', 'alias', 'data'].includes(item.name)));
  } finally { await f.close(); }
});

test('Rust project CRUD preserves folder ownership, revision fences, exact replay and files on delete', async () => {
  const f = await fixture();
  try {
    const body = { id: randomUUID(), name: 'Build docs', description: 'Local project', folders: [{ mode: 'new' }, { mode: 'existing', path: f.workspace }], members: [{ agent: f.agent, write: true }], confirmWrite: true };
    await f.request('/api/projects', { ...body, confirmWrite: false }, 400);
    const p = await f.request('/api/projects', body);
    const path = `/api/projects/${p.id}`;
    assert.equal(p.folders.length, 2);
    assert(existsSync(p.folders[0]));
    assert.equal(execFileSync('git', ['rev-parse', '--show-toplevel'], { cwd: p.folders[0], encoding: 'utf8' }).trim(), p.folders[0]);
    assert.equal(execFileSync('git', ['log', '--format=%s'], { cwd: p.folders[0], encoding: 'utf8' }).trim(), 'Start project');
    assert(!existsSync(join(p.folders[0], '.git', 'hooks', 'pre-commit.sample')));
    assert(!existsSync(join(f.workspace, '.git')));
    writeFileSync(join(p.folders[0], 'keep.txt'), 'keep');
    assert.deepEqual(await f.request('/api/projects', body), p);
    await f.request('/api/projects', { ...body, name: 'Other' }, 409);
    assert.deepEqual(p.members, body.members);

    const edit = { id: randomUUID(), revision: 1, name: 'Renamed', members: [{ agent: f.agent, write: false }] };
    const changed = await f.request(path + '/edit', edit);
    assert.equal(changed.revision, 2);
    assert.equal(changed.name, 'Renamed');
    assert.equal(changed.sections[0].name, 'general');
    assert.deepEqual(changed.members, edit.members);
    assert.deepEqual(await f.request(path + '/edit', edit), changed);
    await f.request(path + '/edit', { id: randomUUID(), revision: 1, name: 'Stale' }, 409);
    rmSync(f.workspace, { recursive: true });
    assert.deepEqual(await f.request('/api/projects', body), p);
    assert.deepEqual(await f.request(path + '/edit', { members: edit.members, name: edit.name, revision: edit.revision, id: edit.id }), changed);
    await f.restart();
    assert.deepEqual(await f.request('/api/projects', body), p);
    const remove = { id: randomUUID(), revision: 2, confirmed: true };
    assert.deepEqual(await f.request(path + '/delete', remove), { id: p.id, deleted: true });
    assert.deepEqual(await f.request(path + '/delete', remove), { id: p.id, deleted: true });
    assert.equal(readFileSync(join(p.folders[0], 'keep.txt'), 'utf8'), 'keep');
    assert(!(await f.request('/api/projects')).projects.some((project: { id: string }) => project.id === p.id));
    await f.request(path, undefined, 404);
    const db = testDatabase(f.options.database, { readOnly: true });
    try {
      assert.equal(db.prepare('SELECT archived FROM channels WHERE id=?').get(p.channel)?.archived, 1);
      assert.equal(db.prepare('SELECT count(*) n FROM channel_members WHERE channel=?').get(p.channel)?.n, 0);
    } finally { db.close(); }
  } finally { await f.close(); }
});

test('Rust project grants resolve by room or selected side thread folder and fail closed after revocation', async () => {
  const f = await fixture();
  try {
    const other = join(f.directory, 'other');
    mkdirSync(other);
    writeFileSync(join(other, 'read.txt'), 'second folder content');
    const p = await f.request('/api/projects', { id: randomUUID(), name: 'Scope', folders: [{ mode: 'existing', path: f.workspace }, { mode: 'existing', path: other }], members: [{ agent: f.agent, write: false }] });
    const direct = await f.send(`/api/agents/${f.agent}/turns`, 'READ');
    assert.equal(direct.status, 'done');
    assert.doesNotMatch(direct.reply, /project bytes|second folder content/);
    assert.match((await f.send(`/api/channels/${p.channel}/turns`, 'READ')).reply, /project bytes/);
    const side = await f.request(`/api/projects/${p.id}/thread`, { id: randomUUID(), revision: 1, agent: f.agent, folder: other, name: 'Second folder' });
    const path = `/api/threads/${side.threads[0].id}/turns`;
    assert.match((await f.send(path, 'READ')).reply, /second folder content/);
    assert.equal((await f.send(path, 'WRITE')).status, 'failed');
    assert(!existsSync(join(other, 'made.txt')));
    await f.request(`/api/projects/${p.id}/edit`, { id: randomUUID(), revision: 2, members: [] });
    const revoked = await f.send(path, 'READ');
    assert.equal(revoked.status, 'failed');
    assert.match(revoked.error, /not a project member/i);
  } finally { await f.close(); }
});

test('built-in CodeGraph indexes projects and serves API turns without a CLI', async t => {
  const f = await fixture();
  try {
    const projects = await f.request('/api/projects');
    if (!projects.codegraphAvailable) return t.skip('requires Linux with bubblewrap');
    const source = join(f.workspace, 'fixture.ts');
    writeFileSync(source, 'export function projectFixture() { return 42; }\n');
    writeFileSync(join(f.directory, 'outside.ts'), 'export function outsideFixture() {}\n');
    symlinkSync(join(f.directory, 'outside.ts'), join(f.workspace, 'outside.ts'));
    const path = `/api/projects/${f.project.id}`;
    assert.equal(f.project.codegraphEnabled, true);
    // First indexing can happen before the fixture file is written. Attach a
    // fresh folder so readiness proves indexing occurred before any turn.
    const second = join(f.directory, 'codegraph-source');
    mkdirSync(second);
    writeFileSync(join(second, 'second.ts'), 'export const secondFixture = true;\n');
    const changed = await f.request(path + '/edit', {
      id: randomUUID(), revision: f.project.revision, folders: [f.workspace, second],
    });
    let status;
    const deadline = Date.now() + 15000;
    do {
      status = (await f.request(path)).codegraphIndex;
      if (status.state !== 'queued') break;
      await new Promise(resolve => setTimeout(resolve, 100));
    } while (Date.now() < deadline);
    assert.equal(status.state, 'ready', JSON.stringify(status));
    assert(status.files >= 1);
    const turns = `/api/channels/${f.project.channel}/turns`;
    const first = await f.send(turns, 'EXPLORE');
    assert.equal(first.status, 'done', JSON.stringify(first.error));
    assert.match(first.reply, /projectFixture\(\) \{ return 42; \}/);
    assert.doesNotMatch(first.reply, /outsideFixture/);
    assert(!existsSync(join(f.workspace, '.codegraph')));

    writeFileSync(source, 'export function projectFixture() { return 43; }\n');
    assert.match((await f.send(turns, 'EXPLORE')).reply, /return 43/);
    const legacy = { id: randomUUID(), revision: changed.revision };
    const enabled = await f.request(path + '/edit', { ...legacy, codegraphEnabled: false });
    assert.equal(enabled.codegraphEnabled, true);
    assert.equal((await f.request(path + '/edit', legacy)).revision, enabled.revision);
    assert.equal((await f.request(path)).codegraphIndex.state, 'ready');
    assert.match((await f.send(turns, 'EXPLORE')).reply, /return 43/);
    await f.stop();
    const db = testDatabase(f.options.database);
    try {
      db.prepare('UPDATE local_projects SET codegraph_enabled=0 WHERE id=?').run(f.project.id);
      db.prepare(`UPDATE local_project_commands SET
        request=json_set(request,'$.codegraphEnabled',json('false')),
        result=json_set(result,'$.codegraphEnabled',json('false')) WHERE id=?`).run(legacy.id);
    } finally { db.close(); }
    await f.restart();
    assert.equal((await f.request(path + '/edit', legacy)).codegraphEnabled, true);
    assert.match((await f.send(turns, 'EXPLORE')).reply, /return 43/);
  } finally { await f.close(); }
});

async function fixture() {
  const directory = scratch(), workspace = join(directory, 'workspace'), data = join(directory, 'data');
  mkdirSync(workspace);
  mkdirSync(data);
  writeFileSync(join(workspace, 'read.txt'), 'project bytes');
  const calls: any[] = [];
  const provider = createServer(async (req, res) => {
    const chunks = [];
    for await (const chunk of req)
      chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks).toString());
    calls.push(body);
    const latest = [...body.messages].reverse().find((m: any) => m.role === 'user')?.content;
    const name = latest === 'READ' ? 'read_workspace_file' : latest === 'WRITE' ? 'create_workspace_file' : latest === 'GIT' ? 'git_status' : latest === 'EXPLORE' ? 'codegraph_explore' : '';
    const tool = body.messages.at(-1)?.role === 'tool';
    res.end(JSON.stringify({ choices: [tool || !name ? { finish_reason: 'stop', message: { content: tool ? JSON.stringify(body.messages.at(-1).content) : 'No tools' } } : { finish_reason: 'tool_calls', message: { tool_calls: [{ id: randomUUID(), type: 'function', function: { name, arguments: JSON.stringify(name === 'read_workspace_file' ? { path: 'read.txt' } : name === 'create_workspace_file' ? { path: 'made.txt', text: 'made' } : name === 'codegraph_explore' ? { query: 'projectFixture' } : {}) } }] } }] }));
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address() as any;
  const agent = randomUUID(), options = { database: join(data, 'state.sqlite'), baseURL: `http://127.0.0.1:${address.port}/v1`, projectRoot: join(directory, 'Projects') };
  let app = await startApp(options, { directory, servers: [provider] }), stopped = false;
  const request = async (path: string, body?: unknown, status = 200, headers: Record<string, string> = {}) => {
    const response = await fetch(app.origin + path, { method: body ? 'POST' : 'GET', headers: { Origin: app.origin, 'Content-Type': 'application/json', ...headers }, body: body ? JSON.stringify(body) : undefined });
    const result = await response.json();
    assert.equal(response.status, status, JSON.stringify(result));
    return result;
  };
  await request('/api/agents', { runtime: 'api', id: agent, name: 'Coder', model: 'fixture', instructions: 'Help' }, 201);
  const project = await request('/api/projects', { id: randomUUID(), name: 'Project', folders: [{ mode: 'existing', path: workspace }], members: [{ agent, write: false }] });
  const send = async (path: string, text: string) => {
    const id = randomUUID();
    await request(path, { id, text }, 202);
    for (let i = 0; i < 300; i++) {
      const turn = (await request(path)).find((row: any) => row.id === id);
      if (['done', 'failed'].includes(turn.status))
        return turn;
      await new Promise(resolve => setTimeout(resolve, 10));
    }
    assert.fail('Turn timed out');
  };
  return { directory, workspace, agent, project, request, send, calls, options, get app() {
      return app;
    }, async stop() { if (!stopped) {await app.close();stopped=true;}
    }, async restart(extra = {}) {
      if (!stopped) await app.close();
      app = await startApp({ ...options, ...extra }); stopped=false;
    }, async close() {
      if (!stopped) await app.close();
      provider.closeAllConnections();
      await new Promise<void>(resolve => provider.close(() => resolve()));
      rmSync(directory, { recursive: true, force: true });
    } };
}
test('project HTTP Owner rules, direct-chat isolation, read/write enforcement, env fallback, per-folder Git and restart receipts', async () => {
  const f = await fixture();
  try {
    await f.request('/api/projects', undefined, 403, { Authorization: 'Bearer peer' });
    await f.request('/api/projects', undefined, 403, { Origin: 'https://foreign.invalid' });
    await f.request('/api/projects', { id: randomUUID(), name: 'Rejected', folders: [{ mode: 'new' }] }, 403, { 'Content-Type': 'text/plain' });
    assert.equal(await new Promise((resolve, reject) => get(f.app.origin + '/api/projects', { headers: { Host: 'foreign.invalid' } }, res => {
      res.resume();
      resolve(res.statusCode);
    }).on('error', reject)), 403);
    const direct = `/api/agents/${f.agent}/turns`, room = `/api/channels/${f.project.channel}/turns`;
    const managed = join(dirname(f.options.database), 'agent-workspaces', f.agent);
    writeFileSync(join(managed, 'read.txt'), 'agent workspace bytes');
    const directRead = await f.send(direct, 'READ');
    assert.equal(directRead.status, 'done', directRead.error);
    assert.match(directRead.reply, /agent workspace bytes/);
    assert.doesNotMatch(directRead.reply, /project bytes/);
    assert.equal(f.calls.at(-1).tools.some((t: any) => t.function.name === 'read_workspace_file'), true);
    const read = await f.send(room, 'READ');
    assert.equal(read.status, 'done', read.error);
    assert.match(read.reply, /project bytes/);
    assert.equal((await f.send(room, 'WRITE')).status, 'failed');
    assert(!existsSync(join(f.workspace, 'made.txt')));
    const body = { id: randomUUID(), revision: 1, members: [{ agent: f.agent, write: true }], confirmWrite: true };
    const edited = await f.request(`/api/projects/${f.project.id}/edit`, body);
    assert.equal(edited.revision, 2);
    assert.equal((await f.send(room, 'WRITE')).status, 'done');
    assert.equal(readFileSync(join(f.workspace, 'made.txt'), 'utf8'), 'made');
    execFileSync('git', ['init', '-q'], { cwd: f.workspace });
    const git = await f.request(`/api/projects/${f.project.id}/git`);
    assert.equal(git.isRepo, true);
    assert.equal(git.dirtyCount, 2);
    assert.equal((await f.send(room, 'GIT')).status, 'done');
    const second=join(f.directory,'second-repo');mkdirSync(second);execFileSync('git',['init','-q'],{cwd:second});writeFileSync(join(second,'second.txt'),'second');
    const multi=await f.request('/api/projects',{id:randomUUID(),name:'Multiple repositories',folders:[{mode:'existing',path:f.workspace},{mode:'existing',path:second}],members:[{agent:f.agent,write:false}]});
    const otherGit=await f.request(`/api/projects/${multi.id}/git?`+new URLSearchParams({folder:second}));assert.equal(otherGit.dirtyCount,1);assert.equal(otherGit.changedFiles[0].path,'second.txt');
    const side=await f.request(`/api/projects/${multi.id}/thread`,{id:randomUUID(),revision:1,agent:f.agent,folder:second,name:'Second repository'});const gitTurn=await f.send(`/api/threads/${side.threads[0].id}/turns`,'GIT');assert.equal(gitTurn.status,'done');assert.match(gitTurn.reply,/second.txt/);
    await f.request(`/api/projects/${multi.id}/delete`,{id:randomUUID(),revision:2,confirmed:true});assert(existsSync(join(second,'second.txt')));
    await f.restart();
    assert.deepEqual(await f.request(`/api/projects/${f.project.id}/edit`, body), edited);
    assert.equal((await f.request('/api/projects')).projects.length, 1);
    await f.restart({ workspaces: [{ agent: f.agent, directory: f.workspace, write: false }] });
    const envRead = await f.send(direct, 'READ');
    assert.equal(envRead.status, 'done', envRead.error);
    assert.match(envRead.reply, /project bytes/);
    const deleted = await f.request(`/api/projects/${f.project.id}/delete`, { id: randomUUID(), revision: 2, confirmed: true });
    assert.equal(deleted.deleted, true);
    assert(existsSync(join(f.workspace, 'made.txt')));
    assert.equal((await f.request('/api/projects')).projects.length, 0);
    assert((await f.request(room)).some((turn: any) => turn.id === read.id));
  }
  finally {
    await f.close();
  }
});
test('project harness cwd and read-only native escalation denial; direct chat uses the managed folder', async () => {
  const f = await fixture();
  try {
    const binary = join(f.directory, 'fake-harness'), source = readFileSync(new URL('./fixtures/fake-harness.mjs', import.meta.url), 'utf8');
    writeFileSync(binary, `#!${process.execPath}\n` + source.split('\n').slice(1).join('\n'));
    chmodSync(binary, 0o700);
    await f.restart({ harness: { executables: { codex: binary, claude: binary }, issuer: { mintAgentSession: () => ({ id: randomUUID(), token: 'test-mcp-session-token' }), revokeAgentSession: () => {
          } } } });
    await f.request(`/api/agents/${f.agent}/edit`, { id: randomUUID(), revision: 1, name: 'Coder', model: 'fixture', instructions: 'Help', runtime: 'codex', confirmRuntime: true });
    for (const runtime of ['codex', 'claude']) {
      if (runtime === 'claude')
        await f.request(`/api/agents/${f.agent}/edit`, { id: randomUUID(), revision: 2, name: 'Coder', model: 'fixture', instructions: 'Help', runtime, confirmRuntime: true });
      const done = await f.send(`/api/channels/${f.project.channel}/turns`, 'READ_ONLY_WRITE');
      assert.equal(done.status, 'done', done.error);
      assert.equal(done.reply, 'denied');
      assert(existsSync(join(f.workspace, 'fake-log.jsonl')));
      assert.match(readFileSync(join(f.workspace, 'fake-log.jsonl'), 'utf8'), runtime === 'codex' ? /thread\/start/ : /"type":"init"/);
      assert.equal(done.harnessApprovals.length, 0);
      const direct = await f.send(`/api/agents/${f.agent}/turns`, 'Direct chat');
      assert.equal(direct.status, 'done', direct.error);
      const log = join(dirname(f.options.database), 'agent-workspaces', f.agent, 'fake-log.jsonl');
      assert.match(readFileSync(log, 'utf8'), runtime === 'codex' ? /thread\/start/ : /"type":"init"/);
    }
  }
  finally {
    await f.close();
  }
});
test('Rust managed folder retry recovers durable intent after transaction failure without deleting files', async () => {
  const f = await fixture();
  const db = testDatabase(f.options.database);
  try {
    const body = { id: randomUUID(), name: 'Retry folder', folders: [{ mode: 'new' }] };
    db.exec("CREATE TRIGGER reject_project BEFORE INSERT ON local_projects BEGIN SELECT RAISE(ABORT,'fixture failure'); END;");
    await f.request('/api/projects', body, 500);
    const folder = join(f.options.projectRoot, 'retry-folder');
    assert(existsSync(folder));
    writeFileSync(join(folder, 'retained.txt'), 'retained');
    db.exec('DROP TRIGGER reject_project');
    const p = await f.request('/api/projects', body);
    assert.equal(p.folders[0], folder);
    assert.equal(readFileSync(join(folder, 'retained.txt'), 'utf8'), 'retained');
    assert.deepEqual(await f.request('/api/projects', body), p);
  } finally {
    db.close();
    await f.close();
  }
});

test('standalone and P2 migration preserves channel IDs, history, read state, memory, files and legacy sidebar through restart and backup restore', async () => {
  const f = await fixture();
  let snapshot: Awaited<ReturnType<typeof workspaceSnapshot>> | undefined, legacySnapshot: Awaited<ReturnType<typeof workspaceSnapshot>> | undefined;
  let restored: Awaited<ReturnType<typeof startApp>> | undefined;
  try {
    const room = randomUUID(), turn = randomUUID(), file = randomUUID();
    const layout = await f.request('/api/sidebar');
    const legacy = await f.request('/api/sidebar', { id: randomUUID(), revision: layout.revision, sections: [{id: randomUUID(), name: 'Legacy labels', agents: [f.agent]}] });
    await f.stop();
    const db = testDatabase(f.options.database);
    // Recreate pre-migration standalone and P2 data in their historical schema.
    db.exec('DROP TABLE local_project_sections');
    db.prepare('INSERT INTO channels(id,name,instructions,lead,activity,read_position) VALUES (?,?,?,?,12,9)').run(room, 'Acme', 'Plan Acme', f.agent);
    db.prepare('INSERT INTO channel_members VALUES (?,?)').run(room, f.agent);
    db.prepare("INSERT INTO turns(id,agent,channel,text,reply,status,created) VALUES (?,?,?,?,?,'done',?)").run(turn, f.agent, room, 'Retained brief', 'Retained answer', new Date().toISOString());
    db.prepare('INSERT INTO files(id,scope,name,digest,bytes,created) VALUES (?,?,?,?,?,?)').run(file, `channels:${room}`, 'retained.txt', 'fixture', Buffer.from('retained bytes'), new Date().toISOString());
    db.prepare('UPDATE channels SET activity=12,read_position=9 WHERE id=?').run(room);
    db.prepare('INSERT INTO memories(scope,id,text,essential,revision,removed) VALUES (?,?,?,1,1,0)').run(`channels:${room}`,randomUUID(),'Retained team memory');
    const retainedChannel = db.prepare('SELECT * FROM channels WHERE id=?').get(room);
    const retainedTurn = db.prepare('SELECT * FROM turns WHERE id=?').get(turn);
    const retainedFile = db.prepare('SELECT * FROM files WHERE id=?').get(file);
    legacySnapshot=await workspaceSnapshot(db);
    db.close();
    await f.restart();
    const projects = (await f.request('/api/projects')).projects;
    const migrated = projects.find((p: any) => p.name === 'Acme');
    assert(migrated); assert.equal(migrated.channel, room); assert.equal(migrated.sections[0].id, room);
    assert.equal(migrated.sections[0].name, 'general'); assert.equal(migrated.sections[0].activity, 12); assert.equal(migrated.sections[0].read_position, 9);
    assert.deepEqual(migrated.folders, []); assert.deepEqual(migrated.members, [{agent:f.agent,write:false}]);
    const p2 = projects.find((p: any) => p.id === f.project.id);
    assert.equal(p2.channel,f.project.channel);assert.equal(p2.sections[0].name,'general');assert.deepEqual(p2.folders,f.project.folders);assert.deepEqual(p2.members,f.project.members);
    const savedMemory = await f.request(`/api/channels/${room}/memory`);
    assert.deepEqual(await f.request('/api/sidebar'),legacy);
    await f.restart();
    const restartedProjects = (await f.request('/api/projects')).projects;
    assert.deepEqual(restartedProjects.map(projectConfiguration),projects.map(projectConfiguration));
    await f.stop();
    const source = testDatabase(f.options.database);
    try {
      assert.deepEqual(source.prepare('SELECT * FROM turns WHERE id=?').get(turn),retainedTurn);
      assert.deepEqual(source.prepare('SELECT * FROM files WHERE id=?').get(file),retainedFile);
      const actual = source.prepare('SELECT * FROM channels WHERE id=?').get(room)!;
      assert.deepEqual({...actual,project:null},{...retainedChannel});
      snapshot=await workspaceSnapshot(source);
    } finally {source.close();}
    const destination=join(f.directory,'restored.sqlite');restoreWorkspace(snapshot.file,destination);
    restored=await startApp({...f.options,database:destination});
    assert.deepEqual((await fetch(restored.origin+'/api/projects').then(r=>r.json())).projects.map(projectConfiguration),projects.map(projectConfiguration));
    const history=await fetch(restored.origin+`/api/channels/${room}/turns`).then(r=>r.json());
    assert.equal(history[0].id,turn);assert.equal(history[0].reply,'Retained answer');
    assert.deepEqual(await fetch(restored.origin+`/api/channels/${room}/memory`).then(r=>r.json()),savedMemory);
    assert.deepEqual(await fetch(restored.origin+'/api/sidebar').then(r=>r.json()),legacy);
    assert.equal(await fetch(restored.origin+`/api/channels/${room}/files/${file}`).then(r=>r.text()),'retained bytes');
    await restored.close();restored=undefined;
    const oldDestination=join(f.directory,'legacy-restored.sqlite');restoreWorkspace(legacySnapshot.file,oldDestination);
    restored=await startApp({...f.options,database:oldDestination});
    const upgraded=(await fetch(restored.origin+'/api/projects').then(r=>r.json())).projects;
    assert.equal(upgraded.find((p:any)=>p.name==='Acme').sections[0].id,room);
    assert.equal(upgraded.find((p:any)=>p.id===f.project.id).sections[0].id,f.project.channel);
    assert.equal((await fetch(restored.origin+`/api/channels/${room}/turns`).then(r=>r.json()))[0].id,turn);
    assert.deepEqual(await fetch(restored.origin+`/api/channels/${room}/memory`).then(r=>r.json()),savedMemory);
    assert.deepEqual(await fetch(restored.origin+'/api/sidebar').then(r=>r.json()),legacy);
  } finally {await restored?.close();snapshot?.cleanup();legacySnapshot?.cleanup();await f.close();}
});

test('section commands require revisions and request IDs, inherit members, reorder, archive and replay across restart', async () => {
  const f=await fixture();
  try {
    const project=f.project, path=`/api/projects/${project.id}/sections`;
    await f.request(path,{id:randomUUID(),revision:1,name:'general'},409);
    await f.request(path,{id:randomUUID(),revision:1,name:'design'},403,{Authorization:'Bearer peer'});
    await f.request(path,{id:randomUUID(),revision:1,name:'design',members:[]},400);
    const command={id:randomUUID(),revision:1,name:'design'};
    const created=await f.request(path,command,201), design=created.sections.find((s:any)=>s.name==='design');
    assert.equal(created.revision,2);assert.equal(design.revision,1);assert.notEqual(design.id,project.channel);
    assert.deepEqual((await f.request('/api/channels')).find((c:any)=>c.id===design.id).members,[f.agent]);
    assert.deepEqual(await f.request(path,command,201),created);
    await f.request(path,{...command,name:'backend'},409);
    await f.request(path,{id:randomUUID(),revision:1,name:'backend'},409);
    const editPath=`${path}/${design.id}/edit`, edit={id:randomUUID(),revision:2,name:'frontend',position:0};
    const reordered=await f.request(editPath,edit);
    assert.deepEqual(reordered.sections.map((s:any)=>s.name),['frontend','general']);
    assert.equal(reordered.sections[0].revision,2);assert.equal(reordered.sections[1].revision,2);
    await f.request(editPath,{id:randomUUID(),revision:3,archived:'yes'},400);
    await f.request(editPath,{id:randomUUID(),revision:3,position:'0'},400);
    await f.request(`/api/projects/${randomUUID()}/sections/${design.id}/edit`,{id:randomUUID(),revision:3,name:'wrong'},404);
    const turn=await f.send(`/api/channels/${design.id}/turns`,'READ');
    assert.equal(turn.status,'done');assert.match(turn.reply,/project bytes/);assert(turn.room,'Section uses H2 room engine');
    const archived=await f.request(editPath,{id:randomUUID(),revision:3,archived:true});
    assert.equal(archived.sections[0].archived,true);
    assert.equal((await f.request(`/api/channels/${design.id}/turns`))[0].id,turn.id);
    await f.request(`/api/channels/${design.id}/turns`,{id:randomUUID(),text:'Cannot send'},409);
    await f.restart();assert.deepEqual(await f.request(editPath,edit),reordered);
    assert.equal((await f.request(path))[0].archived,true);
    const restored=await f.request(editPath,{id:randomUUID(),revision:4,archived:false});
    assert.equal(restored.revision,5);
    const removed=await f.request(`/api/projects/${project.id}/edit`,{id:randomUUID(),revision:5,members:[]});
    assert.equal(removed.revision,6);
    for(const id of [design.id,project.channel])assert.deepEqual((await f.request('/api/channels')).find((c:any)=>c.id===id).members,[]);
  } finally {await f.close();}
});

test('folderless project and side threads isolate host grants, accept folders later, and archive retains configuration', async () => {
  const f=await fixture();
  try {
    const create={id:randomUUID(),name:'No folders',members:[{agent:f.agent,write:false}]};
    let project=await f.request('/api/projects',create);
    assert.deepEqual(project.folders,[]);assert.equal(project.sections[0].name,'general');
    assert.deepEqual(await f.request('/api/projects',create),project);
    const path=`/api/channels/${project.channel}/turns`;
    await f.restart({workspaces:[{agent:f.agent,directory:f.workspace,write:true}]});
    assert.equal((await f.send(`/api/agents/${f.agent}/turns`,'READ')).status,'done');
    assert.equal((await f.send(path,'READ')).status,'failed','No implicit host binding inside folderless sections');
    project=await f.request(`/api/projects/${project.id}/thread`,{id:randomUUID(),revision:1,agent:f.agent,name:'Focused'});
    const thread=project.threads[0];assert.equal(thread.folder,'');
    assert.equal((await f.send(`/api/threads/${thread.id}/turns`,'READ')).status,'failed');
    project=await f.request(`/api/projects/${project.id}/edit`,{id:randomUUID(),revision:2,folders:[f.workspace]});
    assert.equal((await f.send(path,'READ')).status,'done');
    assert.equal((await f.send(`/api/threads/${thread.id}/turns`,'READ')).status,'done');
    const other=await f.request('/api/projects',{id:randomUUID(),name:'Separate',members:[{agent:f.agent,write:false}]});
    assert.equal((await f.send(`/api/channels/${other.channel}/turns`,'READ')).status,'failed');
    const archive={id:randomUUID(),revision:3,confirmed:true};
    const archived=await f.request(`/api/projects/${project.id}/archive`,archive);
    assert.equal(archived.archived,true);assert.deepEqual(archived.folders,[f.workspace]);assert.deepEqual(archived.members,create.members);
    assert(!(await f.request('/api/projects')).projects.some((p:any)=>p.id===project.id));
    assert.deepEqual(await f.request(`/api/projects/${project.id}/archive`,archive),archived);
    await f.request(path,{id:randomUUID(),text:'Archived'},409);
    assert.equal((await f.request(`/api/projects/${project.id}`)).threads[0].id,thread.id);
    assert(existsSync(join(f.workspace,'read.txt')));
  } finally {await f.close();}
});

function projectConfiguration(project: LocalProject) {
  const configuration = { ...project };
  delete configuration.codegraphIndex;
  return configuration;
}

test('Rust project migration rolls back ownership and retries without touching standalone data', async () => {
  const f = await fixture();
  await f.stop();
  const db = testDatabase(f.options.database);
  try {
    const room = randomUUID();
    db.prepare('INSERT INTO channels(id,name,instructions,lead) VALUES (?,?,?,?)').run(room, 'Retain', 'History', f.agent);
    db.prepare('INSERT INTO channel_members VALUES (?,?)').run(room, f.agent);
    db.exec("CREATE TRIGGER fail_migration BEFORE INSERT ON local_project_sections BEGIN SELECT RAISE(ABORT,'interrupted migration'); END");
    await assert.rejects(f.restart(), /Project migration failed/);
    assert.equal(db.prepare('SELECT COUNT(*) AS n FROM local_projects').get()?.n, 1);
    assert.equal(db.prepare('SELECT project FROM channels WHERE id=?').get(room)?.project, null);
    assert.equal(db.prepare('SELECT count(*) n FROM channel_members WHERE channel=?').get(room)?.n, 1);
    db.exec('DROP TRIGGER fail_migration');
    await f.restart();
    const before = await f.request('/api/projects');
    await f.restart();
    const after = await f.request('/api/projects');
    assert.deepEqual(
      { ...after, projects: after.projects.map(projectConfiguration) },
      { ...before, projects: before.projects.map(projectConfiguration) },
    );
    const project = before.projects.find((p: { id: string }) => p.id === room);
    assert.equal(project.sections[0].id, room);
    assert.equal(project.sections[0].name, 'general');
  } finally {
    db.close();
    await f.close();
  }
});

test('Rust migrates a standalone channel with a colliding ID into its own project', async () => {
  const f = await fixture();
  try {
    await f.stop();
    const db = testDatabase(f.options.database);
    try {
      db.prepare('INSERT INTO channels(id,name,instructions,lead) VALUES (?,?,?,?)').run(f.project.id, 'Standalone collision', 'Separate work', f.agent);
      db.prepare('INSERT INTO channel_members VALUES (?,?)').run(f.project.id, f.agent);
    } finally { db.close(); }
    await f.restart();
    const projects = await f.request('/api/projects');
    const migrated = projects.projects.find((project: { name: string }) => project.name === 'Standalone collision');
    assert(migrated);
    assert.notEqual(migrated.id, f.project.id);
    assert.equal(migrated.sections[0].id, f.project.id);
    const original = projects.projects.find((project: LocalProject) => project.id === f.project.id);
    assert(original);
    assert.deepEqual(projectConfiguration(original), f.project);
    await f.restart();
    const after = await f.request('/api/projects');
    assert.deepEqual(
      { ...after, projects: after.projects.map(projectConfiguration) },
      { ...projects, projects: projects.projects.map(projectConfiguration) },
    );
  } finally { await f.close(); }
});

test('Rust project members retain added order through reload and edit', async () => {
  const f = await fixture();
  try {
    const ids = ['ffffffff-ffff-4fff-8fff-ffffffffffff', '11111111-1111-4111-8111-111111111111', f.agent];
    for (const id of ids.slice(0, 2)) {
      await f.request('/api/agents', { runtime: 'api', id, name: id, model: 'fixture', instructions: 'Help' }, 201);
    }
    const members = ids.map(agent => ({ agent, write: false }));
    const project = await f.request('/api/projects', { id: randomUUID(), name: 'Ordered members', members, folders: [] });
    const path = `/api/projects/${project.id}`;
    await f.restart();
    assert.deepEqual((await f.request(path)).members, members);
    const reordered = members.toReversed();
    await f.request(path + '/edit', { id: randomUUID(), revision: project.revision, members: reordered });
    assert.deepEqual((await f.request(path)).members, reordered);
  } finally { await f.close(); }
});

import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chmodSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { testDatabase } from '../database.ts';
import { startApp } from '../../test-bridge.ts';

const database = join(process.env.MINARBOT_DATA_DIR, 'minarbot.sqlite');
const releases = join(process.env.MINARBOT_DATA_DIR, 'releases');
mkdirSync(join(releases, 'feed'), {recursive:true});
writeFileSync(join(releases, 'feed/latest.json'), JSON.stringify({id:'0.2.0-12345678',version:'0.2.0',commit:'12345678'.repeat(5),created:new Date().toISOString(),node:process.version,notes:'Synthetic installer feed',files:{}}));
const fakeHarness = join(process.env.MINARBOT_DATA_DIR, 'fake-harness');
writeFileSync(fakeHarness, `#!${process.execPath}\n` + readFileSync(new URL('../../fixtures/fake-harness.mjs', import.meta.url), 'utf8').split('\n').slice(1).join('\n'));
chmodSync(fakeHarness, 0o700);
const acpHarness = join(process.env.MINARBOT_DATA_DIR, 'fake-acp');
writeFileSync(acpHarness, `#!${process.execPath}\n` + readFileSync(new URL('../../fixtures/fake-acp.mjs', import.meta.url), 'utf8').split('\n').slice(1).join('\n'));
chmodSync(acpHarness, 0o700);
const options = { harness: { executables: { codex: fakeHarness, claude: fakeHarness, hermes: acpHarness, grok: acpHarness, pi: acpHarness, antigravity: '/missing-antigravity', opencode: '/missing-opencode', gemini: '/missing-gemini', kiro: '/missing-kiro' } }, accounts: [{ id: 'st1-stream', name: 'Synthetic stream', provider: 'openai-compatible', baseURL: process.env.OPENAI_BASE_URL, apiKey: process.env.OPENAI_API_KEY, stream: true }], machines: { healthIntervalMs: 500 }, database, updates:{releasesDir:releases}, port: Number(process.env.PORT), baseURL: process.env.OPENAI_BASE_URL, apiKey: process.env.OPENAI_API_KEY, defaultModel: process.env.OPENAI_MODEL, workspaces: JSON.parse(process.env.MINARBOT_WORKSPACES) };
options.projectRoot = process.env.MINARBOT_PROJECT_ROOT;
options.dist = fileURLToPath(new URL('../../dist', import.meta.url));
let app = await startApp(options);
await app.close();
// Seed only this disposable fixture DB; no provider or owner data.
const dataFixture = join(process.env.MINARBOT_DATA_DIR, 'shared-fixture.sqlite');
const shared = testDatabase(dataFixture);
try {
  shared.exec(`
    CREATE TABLE settings_fixture (name TEXT, count INTEGER);
    INSERT INTO settings_fixture VALUES ('Synthetic shared row', 7);
    CREATE TABLE minarbot_owners (name TEXT PRIMARY KEY COLLATE NOCASE, owner TEXT NOT NULL);
    INSERT INTO minarbot_owners VALUES ('settings_fixture', 'settings-fixture');
  `);
} finally { shared.close(); }
const settingsDb = testDatabase(database);
try {
  settingsDb.prepare('UPDATE shared_data SET image=?, revision=1 WHERE id=1').run(readFileSync(dataFixture));
} finally { settingsDb.close(); }
app = await startApp(options);
const peer = await startApp({ database: join(process.env.MINARBOT_DATA_DIR, 'peer.sqlite'), baseURL: process.env.OPENAI_BASE_URL, apiKey: process.env.OPENAI_API_KEY, defaultModel: process.env.OPENAI_MODEL });
async function post(target, path, body) {
  const response = await fetch(target.origin + path, { method: 'POST', headers: { Origin: target.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
  if (!response.ok) throw new Error(`Machine fixture failed: ${path} ${response.status}`);
  return response.json();
}
await post(peer, '/api/environment', { id: randomUUID(), name: 'Studio workstation' });
await post(peer, '/api/agents', {runtime: 'api',  id: randomUUID(), name: 'Remote Iris', model: 'fixture-model', instructions: 'Synthetic peer agent.' });
const pairing = await post(peer, '/api/pairing-codes', { id: randomUUID() });
await post(app, '/api/machines', { id: randomUUID(), origin: peer.origin, code: pairing.code });
console.log(`Minarbot: ${app.origin}`);
process.once('SIGTERM', () => { void Promise.all([app.close(), peer.close()]).finally(() => process.exit()); });

// Parent-owned synthetic acceptance only. Never exposed through application HTTP.
process.on('message', async message => {
  if (message?.kind !== 'seed-legacy-room') return;
  try {
    await app.close();
    const db=testDatabase(database), room=randomUUID(), turn=randomUUID();
    try {
      db.prepare('INSERT INTO channels(id,name,instructions,lead) VALUES (?,?,?,?)').run(room,'Migrated chatroom','Retained project brief',message.agent);
      db.prepare('INSERT INTO channel_members VALUES (?,?)').run(room,message.agent);
      db.prepare("INSERT INTO turns(id,agent,channel,text,reply,status,created) VALUES (?,?,?,?,?,'done',?)").run(turn,message.agent,room,'Original room brief','Original room history stays here.',new Date().toISOString());
    } finally {db.close();}
    app=await startApp(options);
    process.send({kind:'legacy-room-ready',room,turn});
  } catch (error) {process.send({kind:'legacy-room-error',error:error.stack});}
});

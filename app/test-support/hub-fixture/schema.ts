import { createHash, randomUUID } from 'node:crypto';
import { closeSync, fstatSync, openSync, readFileSync, realpathSync, unlinkSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { backup, DatabaseSync } from 'node:sqlite';

export const HUB_SCHEMA_VERSION = 1;
// Same marker and message as crates/minarbot-launcher/src/data_format.rs.
const MINARBOT_APPLICATION_ID = 0x4d4e5231;
const UNMARKED = 'This database does not carry the Minarbot data marker. It was made by Minarbot 0.14.0 or earlier, or by another program. Install fresh: https://github.com/labsiqbal/minarbot#install';
const migration = readFileSync(new URL('../../crates/minarbot-server/src/hub/migrations/001-hub.sql', import.meta.url), 'utf8');
const checksum = createHash('sha256').update(migration).digest('hex');

export function transaction<T>(db: DatabaseSync, work: () => T): T {
  db.exec('BEGIN IMMEDIATE');
  try { const result = work(); db.exec('COMMIT'); return result; }
  catch (error) { db.exec('ROLLBACK'); throw error; }
}

/** Marks a new, empty hub database and refuses one that holds data without the marker. */
function adoptHubData(db: DatabaseSync) {
  if (db.prepare('PRAGMA application_id').get()!.application_id === MINARBOT_APPLICATION_ID) return;
  if (db.prepare('PRAGMA page_count').get()!.page_count !== 0) throw new Error(UNMARKED);
  db.exec(`PRAGMA application_id=${MINARBOT_APPLICATION_ID}`);
}

export function initializeHubSchema(db: DatabaseSync) {
  db.exec('PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000');
  adoptHubData(db);
  const tables = db.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'").all();
  if (tables.length && !tables.some(row => row.name === 'hub_migrations')) throw new Error('Refusing non-hub database.');
  transaction(db, () => {
    db.exec('CREATE TABLE IF NOT EXISTS hub_migrations(version INTEGER PRIMARY KEY, checksum TEXT NOT NULL) STRICT');
    const rows = db.prepare('SELECT * FROM hub_migrations ORDER BY version').all();
    if (rows.length > 1 || rows.some(row => row.version !== HUB_SCHEMA_VERSION || row.checksum !== checksum)) throw new Error('Incompatible hub migration version/checksum. Restore compatible code or snapshot.');
    if (!rows.length) {
      db.exec(migration);
      db.prepare('INSERT INTO hub_migrations VALUES (?,?)').run(HUB_SCHEMA_VERSION, checksum);
      db.prepare("INSERT INTO hub_state(id,authority_generation,state) VALUES(1,?,'active')").run(randomUUID());
    }
    if (db.prepare('PRAGMA foreign_key_check').all().length) throw new Error('Hub foreign key check failed.');
  });
}

// ponytail: one exclusive pilot writer; stale lock needs operator fencing, never automatic stealing.
export function openHubDatabase(filename: string) {
  if (filename === ':memory:') throw new Error('Persistent hub path required.');
  const path = resolve(realpathSync(dirname(filename)), filename.slice(filename.lastIndexOf('/') + 1));
  const lock = `${path}.writer.lock`;
  const descriptor = openSync(lock, 'wx', 0o600);
  let db: DatabaseSync | undefined;
  try {
    // Reject aliases, including existing DB symlinks, before opening a second writer.
    try { if (realpathSync(path) !== path) throw new Error('Hub database aliases are forbidden.'); }
    catch (error) { if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error; }
    const file = openSync(path, 'a', 0o600);
    const links = fstatSync(file).nlink; closeSync(file);
    if (links !== 1) throw new Error('Hub database hard links are forbidden.');
    db = new DatabaseSync(path); initializeHubSchema(db); db.exec('PRAGMA journal_mode=WAL');
    let closed = false;
    return { db, close() { if (closed) return; db!.close(); closeSync(descriptor); unlinkSync(lock); closed = true; } };
  } catch (error) { db?.close(); closeSync(descriptor); unlinkSync(lock); throw error; }
}

export async function backupHub(db: DatabaseSync, destination: string) {
  // Native SQLite backup includes committed WAL content. Caller encrypts off-host copies.
  const file = openSync(destination, 'wx', 0o600); closeSync(file);
  try { await backup(db, destination); }
  catch (error) { unlinkSync(destination); throw error; }
}

/** Call only on a separately restored, exclusively locked DB before serving anything. */
export function fenceRestoredHub(db: DatabaseSync) {
  return transaction(db, () => {
    const generation = randomUUID();
    db.prepare("UPDATE hub_state SET authority_generation=?,state='recovery',revision=revision+1 WHERE id=1").run(generation);
    db.exec("UPDATE invitations SET state='held',revision=revision+1 WHERE state='pending'; UPDATE nodes SET status=CASE WHEN status='revoked' THEN status ELSE 'offline' END,generation=generation+1,connection_generation=connection_generation+1; UPDATE turns SET state='unknown' WHERE state IN('pending','admitted'); DELETE FROM outbox; DELETE FROM action_leases");
    return generation;
  });
}

/** Trusted operator step, after restore checks; nodes must reconnect/reconcile before effects. */
export function activateRestoredHub(db: DatabaseSync, expectedGeneration: string) {
  transaction(db, () => {
    const result = db.prepare("UPDATE hub_state SET state='active',revision=revision+1 WHERE id=1 AND state='recovery' AND authority_generation=?").run(expectedGeneration);
    if (result.changes !== 1) throw new Error('Recovery generation changed.');
  });
}

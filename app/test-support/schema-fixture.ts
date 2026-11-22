import type { DatabaseSync } from 'node:sqlite';

export function transaction<T>(db: DatabaseSync, work: () => T): T {
  db.exec('BEGIN IMMEDIATE');
  try { const value = work(); db.exec('COMMIT'); return value; }
  catch (error) { db.exec('ROLLBACK'); throw error; }
}

export function schemaVersion(db: DatabaseSync): number {
  if (!db.prepare("SELECT 1 FROM sqlite_schema WHERE type='table' AND name='schema_migrations'").get()) return 0;
  return Number(db.prepare('SELECT coalesce(max(version),0) AS version FROM schema_migrations').get()!.version);
}

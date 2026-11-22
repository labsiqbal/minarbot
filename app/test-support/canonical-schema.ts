import { randomUUID } from 'node:crypto';
import { readFileSync } from 'node:fs';
import type { DatabaseSync } from 'node:sqlite';
import { digestCommand } from '../shared/protocol.ts';
import { transaction } from './schema-fixture.ts';

const schema = readFileSync(new URL('./fixtures/canonical-pre-collapse.sql', import.meta.url), 'utf8');

export function installCanonicalFixture(db: DatabaseSync) {
  db.function('minarbot_capability_digest', { deterministic: true }, value => digestCommand(['minarbot-capabilities-v1', JSON.parse(String(value))]));
  transaction(db, () => {
    db.exec(schema);
    db.prepare("INSERT INTO authority_state VALUES (1,?,'active',0,NULL,NULL,1)").run(randomUUID());
  });
}

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { workflowAutosaveDraft } from './src/workflow-editor-state.ts';
import { mkdtempSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { startApp } from './test-bridge.ts';

// The other cases moved to crates/minarbot-e2e/tests/e2e/sa_workflows.rs. This
// one stays because it checks the TypeScript module
// app/src/workflow-editor-state.ts; its server half is ported too
// (docs/test-ledger.md).
test('autosave stops after server canonicalizes condition property order', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'minarbot-autosave-'));
  const app = await startApp({ database: join(dir, 'state.sqlite') });
  async function post(path: string, body: unknown) {
    const response = await fetch(app.origin + path, { method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
    const value = await response.json(); assert(response.ok, JSON.stringify(value)); return value;
  }
  try {
    const agent = await post('/api/agents', { runtime: 'api', id: randomUUID(), name: 'Autosave', model: 'fixture', instructions: 'Test' });
    const graph = { entry: 'check', maxSteps: 2, nodes: [
      { id: 'check', kind: 'condition' as const, operator: 'equals' as const, value: 'yes', yes: 'out', no: 'out' }, { id: 'out', kind: 'output' as const },
    ] };
    const saved = await post(`/api/agents/${agent.id}/workflows`, { id: randomUUID(), revision: 0, name: 'Autosave', graph });
    const local = { ...graph, nodes: [{ id: 'check', kind: 'condition' as const, value: 'yes', yes: 'out', no: 'out', operator: 'contains' as const }, graph.nodes[1]] };
    const draft = workflowAutosaveDraft(saved.name, local, saved, new Set([agent.id])); assert(draft);
    const ack = await post(`/api/workflows/${saved.id}/edit`, { id: randomUUID(), ...draft });
    assert.equal(ack.revision, 2);
    assert.equal(workflowAutosaveDraft(ack.name, local, ack, new Set([agent.id])), null);
  } finally { await app.close(); rmSync(dir, { recursive: true, force: true }); }
});

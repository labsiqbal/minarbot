import { test } from 'node:test';
import assert from 'node:assert/strict';
import { removeWorkflowNode, validateWorkflow, workflowEdges, workflowStepTypeLabel, workflowStepTypes, type WorkflowGraph } from './src/workflows.ts';
import { restoreWorkflowCommand } from './src/workflow-command.ts';
import { randomUUID } from 'node:crypto';
import { editWorkflowHistory, moveWorkflowNode, restoreWorkflowEditorDraft, workflowAutosaveDraft, workflowLayout, workflowUnconditionalCycles, workflowNodeLabels, workflowStepLabel, type WorkflowHistory } from './src/workflow-editor-state.ts';

test('deleting a legacy approval rejection target leaves a saveable graph', () => {
  const graph = validateWorkflow({ entry: 'work', maxSteps: 5, nodes: [
    { id: 'work', kind: 'model', model: 'fixture', prompt: 'Work', next: 'review' },
    { id: 'review', kind: 'approval', prompt: 'Accept?', yes: 'out', no: 'work' },
    { id: 'out', kind: 'output' },
  ] }, new Set());
  const edited = removeWorkflowNode({ ...graph, entry: 'review' }, 'work');
  assert.deepEqual(validateWorkflow(edited, new Set()), edited);
  assert.equal('no' in edited.nodes[0], false);
  assert.equal('no' in graph.nodes[1], true);
  assert.equal(removeWorkflowNode(graph, 'work').entry, '');
  assert.throws(() => validateWorkflow(removeWorkflowNode(graph, 'out'), new Set()));
});

test('model condition validation requires model and question', () => {
  const node = { id: 'check', kind: 'condition', operator: 'model', model: 'fixture', value: 'Accept?', yes: 'out', no: 'out' };
  const graph = { entry: 'check', maxSteps: 2, nodes: [node, { id: 'out', kind: 'output' }] };
  assert.deepEqual(validateWorkflow(graph, new Set()), graph);
  for (const patch of [{ model: '' }, { value: '' }, { next: 'out' }]) assert.throws(() => validateWorkflow({ ...graph, nodes: [{ ...node, ...patch }, graph.nodes[1]] }, new Set()));
});

test('workflow graphs validate bounded loops and terminal paths', () => {
  const input = { entry: 'draft', maxSteps: 12, nodes: [
    { id: 'draft', kind: 'agent', agent: 'a', prompt: 'Draft', next: 'review' },
    { id: 'review', kind: 'approval', prompt: 'Accept draft?', yes: 'check', no: 'draft' },
    { id: 'check', kind: 'condition', operator: 'equals', value: 'ready', yes: 'done', no: 'revise' },
    { id: 'revise', kind: 'model', model: 'fixture', prompt: 'Revise', next: 'review' },
    { id: 'done', kind: 'output' },
  ] };
  const agents = new Set(['a']), graph = validateWorkflow(input, agents);
  assert.deepEqual(graph, input);
  for (const bad of [
    { ...input, maxSteps: 0 }, { ...input, maxSteps: 1001 }, { ...input, maxSteps: 1.5 },
    { ...input, extra: true }, { ...input, entry: 'missing' },
    { ...input, nodes: [...input.nodes, input.nodes[0]] },
    { ...input, nodes: [...input.nodes, { id: 'orphan', kind: 'output' }] },
    { ...input, nodes: [{ id: 'forever', kind: 'agent', agent: 'a', prompt: 'Loop', next: 'forever' }], entry: 'forever' },
    { ...input, nodes: [{ id: 'script', kind: 'shell', command: 'anything' }], entry: 'script' },
  ]) assert.throws(() => validateWorkflow(bad, agents));
  assert.throws(() => validateWorkflow(input, new Set()));
  assert.throws(() => validateWorkflow({ ...input, nodes: [{ ...input.nodes[0], next: 'missing' }, ...input.nodes.slice(1)] }, agents));
});
test('workflow recovery rejects malformed commands and preserves valid retry identity', () => {
  const owner = randomUUID(), agents = new Set([owner]);
  const workflow = { id: randomUUID(), agent: owner, name: 'Saved', revision: 1, graph: { entry: 'out', maxSteps: 1, nodes: [{ id: 'out', kind: 'output' }] } };
  const run = { kind: 'run', workflow, path: `/api/workflows/${workflow.id}/run`, body: { id: randomUUID(), revision: 1, input: 'marker' } };
  const save = { kind: 'save', path: `/api/agents/${owner}/workflows`, body: { id: randomUUID(), revision: 0, name: 'New', graph: workflow.graph } };
  const approve = { kind: 'approve', workflow, path: `/api/workflow-runs/${randomUUID()}/approve`, body: { id: randomUUID(), revision: 2, accepted: true } };
  const restore = { kind: 'restore', workflow, path: `/api/workflows/${workflow.id}/restore`, body: { id: randomUUID(), revision: 1, version: 1 } };
  const schedule = { kind: 'schedule', workflow, path: `/api/workflows/${workflow.id}/schedule`, body: { id: randomUUID(), revision: 0, nextAt: Date.now(), intervalSeconds: null, input: 'scheduled', enabled: false } };
  const retry = { kind: 'retry', workflow, path: `/api/workflow-runs/${randomUUID()}/retry`, body: { id: randomUUID(), revision: 3 } };
  const webhook = { kind: 'webhook', workflow, path: `/api/workflows/${workflow.id}/webhook`, body: { id: randomUUID(), revision: 0, enabled: false, rotate: true } };
  const removeWebhook = { ...webhook, body: { id: randomUUID(), revision: 1, remove: true, route: randomUUID() } };
  const autosave = { ...save, autosave: true, workflow, path: `/api/workflows/${workflow.id}/edit`, body: { ...save.body, revision: 1 } };
  const deletion = { kind: 'delete', workflow, path: `/api/workflows/${workflow.id}/delete`, body: { id: randomUUID(), revision: 1 } };
  for (const command of [run, save, approve, restore, schedule, retry, webhook, removeWebhook, autosave, deletion]) assert.deepEqual(restoreWorkflowCommand(JSON.stringify(command), owner, agents), command);
  assert.equal(restoreWorkflowCommand(null, owner, agents), null);
  for (const command of [{ ...run, autosave: true }, { ...save, autosave: true }, { ...autosave, autosave: 'yes' }, { ...removeWebhook, body: { ...removeWebhook.body, route: 'invalid' } }, { ...webhook, body: { ...webhook.body, secret: 'must-not-persist' } }, { ...webhook, body: { ...webhook.body, rotate: 'yes' } }, { kind: 'run' }, { kind: 'save', path: '/api/agents/x/workflows', body: { id: 'x', name: 'bad', graph: {} } },
    { ...save, body: { ...save.body, graph: {} } }, { ...run, workflow: { ...workflow, agent: randomUUID() } },
    { ...run, path: '/api/agents' }, { ...approve, body: { ...approve.body, accepted: 'yes' } },
    { ...run, body: { ...run.body, revision: -1 } }, { ...run, workflow: { ...workflow, graph: {} } }, { ...restore, body: { ...restore.body, version: '1' } },
    { ...schedule, body: { ...schedule.body, nextAt: 253370764800000 } }, { ...schedule, body: { ...schedule.body, intervalSeconds: 1 } }, { ...schedule, body: { ...schedule.body, enabled: 'yes' } }]) {
    assert.throws(() => restoreWorkflowCommand(JSON.stringify(command), owner, agents));
  }
});


test('workflow editor undo restores removed edges and bounds branching history', () => {
  const graph = validateWorkflow({ entry: 'work', maxSteps: 10, nodes: [
    { id: 'work', kind: 'model', model: 'fixture', prompt: 'Process', next: 'check' },
    { id: 'check', kind: 'condition', operator: 'equals', value: 'yes', yes: 'out', no: 'work' },
    { id: 'out', kind: 'output' },
  ] }, new Set());
  let history: WorkflowHistory = { graph, past: [], future: [] };
  assert.equal(editWorkflowHistory(history, 'undo'), history);
  history = editWorkflowHistory(history, removeWorkflowNode(graph, 'work'));
  assert.throws(() => validateWorkflow(history.graph, new Set()));
  history = editWorkflowHistory(history, 'undo');
  assert.deepEqual(validateWorkflow(history.graph, new Set()), graph);
  history = editWorkflowHistory(history, 'redo'); assert.equal(history.graph.nodes.length, 2);
  history = editWorkflowHistory(history, 'undo');
  history = editWorkflowHistory(history, { ...graph, maxSteps: 15 });
  assert.equal(history.future.length, 0); assert.equal(editWorkflowHistory(history, 'redo'), history);
  for (let index = 0; index < 110; index++) history = editWorkflowHistory(history, { ...history.graph, maxSteps: index + 20 });
  assert.equal(history.past.length, 100);
  const draft = { ...graph, nodes: [...graph.nodes, { id: 'orphan', kind: 'output' as const }] };
  const positions = workflowLayout(draft);
  assert.equal(positions.length, 4); assert.equal(new Set(positions.map(point => `${point.x}:${point.y}`)).size, 4);
  assert(positions.find(point => point.id === 'check')!.x > positions.find(point => point.id === 'work')!.x);
  assert.deepEqual(workflowLayout(draft), positions);
  assert.equal(workflowLayout({ ...draft, entry: '' }).length, 4);
  const self = { entry: 'loop', maxSteps: 1, nodes: [{ id: 'loop', kind: 'model' as const, model: 'fixture', prompt: 'Loop', next: 'loop' }] };
  assert.equal(workflowLayout(self).length, 1);
});


test('workflow positions validate, survive edits and undo, and preserve edges', () => {
  const original = validateWorkflow({ entry: 'work', maxSteps: 2, nodes: [
    { id: 'work', kind: 'model', model: 'fixture', prompt: 'Work', next: 'out' }, { id: 'out', kind: 'output' },
  ] }, new Set());
  const moved = moveWorkflowNode(original, 'work', 501.4, 701.8);
  assert.deepEqual(validateWorkflow(moved, new Set()), moved);
  assert.deepEqual(workflowLayout(moved).find(node => node.id === 'work'), { id: 'work', x: 501, y: 702 });
  assert.deepEqual(workflowEdges(moved.nodes[0]), ['out']);
  const history = editWorkflowHistory({ graph: original, past: [], future: [] }, moved);
  assert.deepEqual(editWorkflowHistory(history, 'undo').graph, original);
  assert.deepEqual(editWorkflowHistory(editWorkflowHistory(history, 'undo'), 'redo').graph, moved);
  assert.deepEqual(removeWorkflowNode(moved, 'work').positions, {});
  assert.equal(moveWorkflowNode(original, 'missing', 100, 100), original);
  assert.equal(moveWorkflowNode(original, 'work', NaN, 100), original);
  assert.deepEqual(moveWorkflowNode(original, 'out', -10, 30000).positions, { out: { x: 64, y: 20000 } });
  for (const positions of [[], null, { missing: { x: 100, y: 100 } }, { work: { x: 0, y: 100 } }, { work: { x: 100, y: 20001 } }, { work: { x: Infinity, y: 100 } }, { work: { x: 100, y: 100, z: 1 } }]) {
    assert.throws(() => validateWorkflow({ ...original, positions }, new Set()));
  }
  const unusual = validateWorkflow({ entry: 'constructor', maxSteps: 1, nodes: [{ id: 'constructor', kind: 'output' }], positions: {} }, new Set());
  assert.equal(workflowLayout(unusual)[0].x, 64);
});


test('autosave only submits valid changes on the acknowledged revision', () => {
  const graph = validateWorkflow({ entry: 'out', maxSteps: 2, nodes: [{ id: 'out', kind: 'output' }] }, new Set());
  const saved = { name: 'Saved', graph, revision: 3 };
  assert.equal(workflowAutosaveDraft('New', graph, undefined, new Set()), null);
  assert.equal(workflowAutosaveDraft('Saved ', graph, saved, new Set()), null);
  for (const name of ['', ' ', 'x'.repeat(81)]) assert.equal(workflowAutosaveDraft(name, graph, saved, new Set()), null);
  assert.equal(workflowAutosaveDraft('Saved', { ...graph, entry: '' }, saved, new Set()), null);
  const changed = moveWorkflowNode(graph, 'out', 900, 700);
  const draft = workflowAutosaveDraft('Renamed ', changed, saved, new Set());
  assert.deepEqual(draft, { name: 'Renamed', graph: changed, revision: 3 });
  const acknowledged = { ...draft!, revision: 4 };
  assert.equal(workflowAutosaveDraft('Renamed', changed, acknowledged, new Set()), null);
  const undone = editWorkflowHistory(editWorkflowHistory({ graph, past: [], future: [] }, changed), 'undo').graph;
  assert.deepEqual(workflowAutosaveDraft('Renamed', undone, acknowledged, new Set()), { name: 'Renamed', graph, revision: 4 });
});


test('local workflow drafts preserve incomplete edits without relaxing execution validation', () => {
  const draft = { name: '', revision: 7, graph: { entry: '', maxSteps: 0, nodes: [
    { id: 'work', kind: 'model', model: '', prompt: '', next: '' },
    { id: 'check', kind: 'condition', operator: 'model', model: '', value: '', yes: '', no: '' },
    { id: 'out', kind: 'output' },
  ], positions: { work: { x: 200, y: 200 } } } };
  assert.deepEqual(restoreWorkflowEditorDraft(JSON.stringify(draft)), draft);
  assert.throws(() => validateWorkflow(draft.graph, new Set()));
  assert.equal(restoreWorkflowEditorDraft(null), null);
  for (const value of [null, [], { ...draft, revision: -1 }, { ...draft, revision: '7' }, { ...draft, name: 'x'.repeat(81) },
    { ...draft, extra: true }, { ...draft, graph: { ...draft.graph, nodes: [null] } },
    { ...draft, graph: { ...draft.graph, nodes: [{ id: 'work', kind: 'shell', command: 'ignored' }] } },
    { ...draft, graph: { ...draft.graph, positions: { work: { x: -1, y: 200 } } } },
    { ...draft, graph: { ...draft.graph, nodes: [...draft.graph.nodes, draft.graph.nodes[0]] } }]) assert.throws(() => restoreWorkflowEditorDraft(JSON.stringify(value)));
  assert.throws(() => restoreWorkflowEditorDraft('x'.repeat(2 * 1024 * 1024 + 1)));
  const history = { graph: restoreWorkflowEditorDraft(JSON.stringify(draft))!.graph, past: [], future: [] };
  const latest = validateWorkflow({ entry: 'out', maxSteps: 1, nodes: [{ id: 'out', kind: 'output' }] }, new Set());
  assert.deepEqual(editWorkflowHistory(editWorkflowHistory(history, latest), 'undo').graph, history.graph);
});


test('saved legacy steps stay visible and can be replaced without offering them as choices', () => {
  for (const kind of ['image', 'asset', 'file-output', 'message-trigger', 'constructor']) {
    const legacy = { id: 'legacy', kind, next: 'out', prompt: 'Keep original settings' };
    const graph = { entry: 'legacy', maxSteps: 2, nodes: [legacy, { id: 'out', kind: 'output' }] } as WorkflowGraph;
    const draft = { name: 'Legacy', graph, revision: 1 };
    assert.equal(Object.hasOwn(workflowStepTypes, kind), false);
    assert.equal(workflowStepTypeLabel(graph.nodes[0].kind), `Unsupported (${kind})`);
    assert.deepEqual(workflowEdges(graph.nodes[0]), []);
    assert.deepEqual(restoreWorkflowEditorDraft(JSON.stringify(draft), graph), draft);
    assert.throws(() => restoreWorkflowEditorDraft(JSON.stringify(draft)));
    assert.throws(() => validateWorkflow(graph, new Set()), /Unsupported workflow step type/);
    assert.equal(workflowAutosaveDraft('Renamed', graph, draft, new Set()), null);
    const repaired = { ...removeWorkflowNode(graph, 'legacy'), entry: 'out' };
    assert.deepEqual(workflowAutosaveDraft(draft.name, repaired, draft, new Set())?.graph, repaired);
  }
});

test('graph view identifies unconditional cycles and latest run attempt without mutating history', () => {
  const graph = { entry: 'a', maxSteps: 5, nodes: [
    { id: 'a', kind: 'model' as const, model: 'fixture', prompt: 'Work', next: 'b' },
    { id: 'b', kind: 'approval' as const, prompt: 'Continue?', yes: 'a' },
    { id: 'out', kind: 'output' as const },
  ] };
  assert.deepEqual(workflowUnconditionalCycles(graph), [['a', 'b']]);
  assert.equal(graph.nodes[0].kind, 'model');
  if (graph.nodes[0].kind !== 'model') assert.fail('Expected model fixture');
  assert.deepEqual(workflowUnconditionalCycles({ ...graph, nodes: [graph.nodes[0], { id: 'b', kind: 'condition', operator: 'equals', value: 'yes', yes: 'a', no: 'out' }, graph.nodes[2]] }), []);
  assert.deepEqual(workflowUnconditionalCycles({ ...graph, nodes: [{ ...graph.nodes[0], next: 'a' }, graph.nodes[2]] }), [['a']]);
  assert.deepEqual(workflowUnconditionalCycles({ ...graph, nodes: [{ ...graph.nodes[0], next: '' }, graph.nodes[2]] }), []);
  const steps = [
    { seq: 4, node: 'a', status: 'done', task_status: 'done', paused: 1 },
    { seq: 2, node: 'a', status: 'failed', task_status: 'done' },
    { seq: 5, node: 'b', status: 'failed', detail: 'rejected' },
  ];
  const before = JSON.stringify(steps), labels = workflowNodeLabels(steps);
  assert.equal(labels.get('a'), 'Completed'); assert.equal(labels.get('b'), 'Rejected'); assert.equal(labels.has('out'), false);
  assert.equal(JSON.stringify(steps), before);
  assert.equal(workflowStepLabel({ seq: 1, node: 'a', status: 'failed', task_status: 'done', paused: 1 }), 'Failed');
  assert.equal(workflowStepLabel({ seq: 1, node: 'a', status: 'waiting', task_status: 'running', paused: 1 }), 'Paused');
  assert.equal(workflowStepLabel({ seq: 1, node: 'a', status: 'waiting', task_status: 'running' }), 'Running');
});

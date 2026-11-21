import { validateWorkflow, validateWorkflowDraft, workflowEdges, type WorkflowGraph, type WorkflowNode } from './workflows.ts';

export function workflowAutosaveDraft(name: string, graph: WorkflowGraph, saved: { name: string; graph: WorkflowGraph; revision: number } | undefined, agents: ReadonlySet<string>) {
  if (!saved || !name.trim() || name.trim().length > 80 || workflowDraftMatches(name, graph, saved, agents)) return null;
  try { return { name: name.trim(), graph: validateWorkflow(graph, agents), revision: saved.revision }; }
  catch { return null; }
}

export function workflowDraftMatches(name: string, graph: WorkflowGraph, saved: { name: string; graph: WorkflowGraph }, agents: ReadonlySet<string>): boolean {
  if (name.trim() !== saved.name) return false;
  try { return JSON.stringify(validateWorkflow(graph, agents)) === JSON.stringify(validateWorkflow(saved.graph, agents)); }
  catch { return JSON.stringify(graph) === JSON.stringify(saved.graph); }
}

export type WorkflowHistory = { graph: WorkflowGraph; past: WorkflowGraph[]; future: WorkflowGraph[] };
export const workflowStatusLabels: Record<string, string> = { ready: 'Ready', waiting: 'Waiting for step', approval: 'Waiting for approval', done: 'Completed', failed: 'Failed', cancelled: 'Cancelled', interrupted: 'Interrupted', queued: 'Queued', running: 'Running' };
export type WorkflowStepView = { seq: number; node: string; status: string; detail?: string | null; paused?: number | null; task_status?: string | null };
export function workflowStepLabel(step: WorkflowStepView): string {
  const status = step.status === 'waiting' ? step.task_status ?? step.status : step.status;
  return step.detail === 'rejected' ? 'Rejected' : step.paused && step.status === 'waiting' ? 'Paused' : workflowStatusLabels[status] ?? status;
}
export function workflowNodeLabels(steps: WorkflowStepView[]): Map<string, string> {
  const latest = new Map<string, WorkflowStepView>();
  for (const step of steps) if (!latest.has(step.node) || latest.get(step.node)!.seq < step.seq) latest.set(step.node, step);
  return new Map([...latest].map(([id, step]) => [id, workflowStepLabel(step)]));
}

export function workflowUnconditionalCycles(graph: WorkflowGraph): string[][] {
  const nodes = new Map(graph.nodes.map(node => [node.id, node])), visited = new Set<string>(), cycles: string[][] = [];
  for (const start of graph.nodes) {
    const path: string[] = [], indices = new Map<string, number>();
    let next: string | undefined = start.id;
    while (next && nodes.has(next) && !visited.has(next)) {
      const cycle = indices.get(next);
      if (cycle !== undefined) { cycles.push(path.slice(cycle)); break; }
      indices.set(next, path.length); path.push(next);
      const node: WorkflowNode = nodes.get(next)!;
      next = node.kind === 'condition' || node.kind === 'output' ? undefined : workflowEdges(node)[0];
    }
    for (const id of path) visited.add(id);
  }
  return cycles;
}
export function editWorkflowHistory(history: WorkflowHistory, action: WorkflowGraph | 'undo' | 'redo'): WorkflowHistory {
  if (action === 'undo') {
    const graph = history.past.at(-1);
    return graph ? { graph, past: history.past.slice(0, -1), future: [history.graph, ...history.future] } : history;
  }
  if (action === 'redo') {
    const graph = history.future[0];
    return graph ? { graph, past: [...history.past, history.graph], future: history.future.slice(1) } : history;
  }
  if (JSON.stringify(action) === JSON.stringify(history.graph)) return history;
  // ponytail: keep the latest 100 graph edits in memory; add durable drafts separately.
  return { graph: action, past: [...history.past.slice(-99), history.graph], future: [] };
}

export function workflowLayout(graph: WorkflowGraph) {
  const nodes = new Map(graph.nodes.map(node => [node.id, node]));
  const levels = new Map<string, number>(), queue: string[] = [];
  if (nodes.has(graph.entry)) { levels.set(graph.entry, 0); queue.push(graph.entry); }
  for (let index = 0; index < queue.length; index++) {
    const id = queue[index];
    for (const next of workflowEdges(nodes.get(id)!)) if (nodes.has(next) && !levels.has(next)) {
      levels.set(next, levels.get(id)! + 1); queue.push(next);
    }
  }
  const orphanLevel = Math.max(0, ...levels.values()) + 1;
  const rows = new Map<number, number>();
  return graph.nodes.map(node => {
    const level = levels.get(node.id) ?? orphanLevel, row = rows.get(level) ?? 0;
    rows.set(level, row + 1);
    const saved = graph.positions && Object.hasOwn(graph.positions, node.id) ? graph.positions[node.id] : undefined;
    return { id: node.id, x: saved?.x ?? 64 + level * 280, y: saved?.y ?? 64 + row * 160 };
  });
}

export function moveWorkflowNode(graph: WorkflowGraph, id: string, x: number, y: number): WorkflowGraph {
  if (!graph.nodes.some(node => node.id === id) || !Number.isFinite(x) || !Number.isFinite(y)) return graph;
  return { ...graph, positions: { ...graph.positions, [id]: { x: Math.max(64, Math.min(20000, Math.round(x))), y: Math.max(64, Math.min(20000, Math.round(y))) } } };
}


export function restoreWorkflowEditorDraft(raw: string | null, saved?: WorkflowGraph): { name: string; graph: WorkflowGraph; revision: number } | null {
  if (raw === null) return null;
  if (raw.length > 2 * 1024 * 1024) throw new Error('Workflow draft is too large.');
  const value = JSON.parse(raw);
  if (!value || typeof value !== 'object' || Array.isArray(value) || Object.keys(value).some(key => !['name', 'graph', 'revision'].includes(key))
    || typeof value.name !== 'string' || value.name.length > 80 || !Number.isSafeInteger(value.revision) || value.revision < 0) throw new Error('Invalid workflow draft.');
  return { name: value.name, graph: validateWorkflowDraft(value.graph, saved), revision: value.revision };
}

import { validateWorkflow, type WorkflowGraph } from './workflows.ts';

export function workflowAutosaveDraft(name: string, graph: WorkflowGraph, saved: { name: string; graph: WorkflowGraph; revision: number } | undefined, agents: ReadonlySet<string>) {
  if (!saved || !name.trim() || name.trim().length > 80 || workflowDraftMatches(name, graph, saved, agents)) return null;
  try { return { name: name.trim(), graph: validateWorkflow(graph, agents), revision: saved.revision }; }
  catch { return null; }
}

function workflowDraftMatches(name: string, graph: WorkflowGraph, saved: { name: string; graph: WorkflowGraph }, agents: ReadonlySet<string>): boolean {
  if (name.trim() !== saved.name) return false;
  try { return JSON.stringify(validateWorkflow(graph, agents)) === JSON.stringify(validateWorkflow(saved.graph, agents)); }
  catch { return JSON.stringify(graph) === JSON.stringify(saved.graph); }
}

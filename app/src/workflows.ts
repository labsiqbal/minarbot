export type WorkflowNode =
  | { id: string; kind: 'parallel'; branches: ({ kind: 'agent'; agent: string; prompt: string } | { kind: 'model'; model: string; prompt: string })[]; next: string }
  | { id: string; kind: 'agent'; agent: string; prompt: string; next: string }
  | { id: string; kind: 'model'; model: string; prompt: string; next: string }
  | { id: string; kind: 'condition'; operator: 'equals' | 'contains'; value: string; yes: string; no: string }
  | { id: string; kind: 'condition'; operator: 'model'; model: string; value: string; yes: string; no: string }
  | { id: string; kind: 'approval'; prompt: string; yes: string; no?: string }
  | { id: string; kind: 'wait'; event: string; timeoutSeconds: number; next: string }
  | { id: string; kind: 'output'; channel?: string; discordChannel?: string };
export type WorkflowGraph = { entry: string; maxSteps: number; nodes: WorkflowNode[]; trigger?: { kind: 'discord'; channel: string }; positions?: Record<string, { x: number; y: number }> };

export function removeWorkflowNode(graph: WorkflowGraph, removed: string): WorkflowGraph {
  return { ...graph, ...(graph.positions ? { positions: Object.fromEntries(Object.entries(graph.positions).filter(([id]) => id !== removed)) } : {}), entry: graph.entry === removed ? '' : graph.entry, nodes: graph.nodes.filter(node => node.id !== removed).map(node => {
    // Legacy approval rejection edges no longer execute or appear in the editor.
    if (node.kind === 'approval') return { id: node.id, kind: node.kind, prompt: node.prompt, yes: node.yes === removed ? '' : node.yes };
    return { ...node, ...('next' in node && node.next === removed ? { next: '' } : {}),
      ...(node.kind === 'condition' ? { yes: node.yes === removed ? '' : node.yes, no: node.no === removed ? '' : node.no } : {}) };
  }) };
}

function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Workflow object is required.');
  return value as Record<string, unknown>;
}
function keys(value: Record<string, unknown>, allowed: string[]) {
  if (Object.keys(value).some(key => !allowed.includes(key))) throw new Error('Unknown workflow field.');
}
function text(value: unknown, max: number, empty = false): string {
  if (typeof value !== 'string' || (!empty && !value.trim()) || value.length > max) throw new Error('Invalid workflow text.');
  return value;
}
function id(value: unknown): string {
  const result = text(value, 64);
  if (!/^[a-zA-Z0-9_-]+$/.test(result)) throw new Error('Invalid workflow step ID.');
  return result;
}
export function workflowEdges(node: WorkflowNode): string[] {
  return node.kind === 'output' ? [] : 'next' in node ? [node.next] : node.kind === 'approval' ? [node.yes] : [node.yes, node.no];
}

export function validateWorkflow(value: unknown, agents: ReadonlySet<string>): WorkflowGraph { return parseWorkflow(value, agents, false); }
export function validateWorkflowDraft(value: unknown): WorkflowGraph { return parseWorkflow(value, new Set(), true); }

function parseWorkflow(value: unknown, agents: ReadonlySet<string>, draft: boolean): WorkflowGraph {
  const edge = (value: unknown) => draft && value === '' ? '' : id(value);
  const discordChannel = (value: unknown) => {
    if (typeof value !== 'string' || !(draft && value === '' || /^[1-9][0-9]{0,19}$/.test(value) && BigInt(value) <= 18446744073709551615n)) throw new Error('Invalid Discord channel ID.');
    return value;
  };
  const graph = object(value); keys(graph, ['entry', 'maxSteps', 'nodes', 'positions', 'trigger']);
  let trigger: WorkflowGraph['trigger'];
  if (graph.trigger !== undefined) {
    const raw = object(graph.trigger); keys(raw, ['kind', 'channel']);
    if (raw.kind !== 'discord') throw new Error('Invalid workflow trigger.');
    trigger = { kind: 'discord', channel: discordChannel(raw.channel) };
  }
  const entry = edge(graph.entry), maxSteps = graph.maxSteps;
  if (typeof maxSteps !== 'number' || !Number.isFinite(maxSteps) || Math.abs(maxSteps) > Number.MAX_SAFE_INTEGER || (!draft && (!Number.isSafeInteger(maxSteps) || maxSteps < 1 || maxSteps > 1000))) throw new Error('Workflow limit must be 1 to 1000 steps.');
  if (!Array.isArray(graph.nodes) || !graph.nodes.length || graph.nodes.length > 64) throw new Error('Workflow must have 1 to 64 steps.');
  const nodes: WorkflowNode[] = graph.nodes.map(raw => {
    const node = object(raw), name = id(node.id);
    switch (node.kind) {
      case 'parallel': {
        keys(node, ['id', 'kind', 'branches', 'next']);
        if (!Array.isArray(node.branches) || node.branches.length < 2 || node.branches.length > 8) throw new Error('Parallel blocks require 2 to 8 branches.');
        const branches = node.branches.map(raw => {
          const branch = object(raw);
          if (branch.kind === 'agent') {
            keys(branch, ['kind', 'agent', 'prompt']);
            if (typeof branch.agent !== 'string' || branch.agent.length > 200 || (!draft && !agents.has(branch.agent))) throw new Error('Workflow agent not found.');
            return { kind: 'agent' as const, agent: branch.agent, prompt: text(branch.prompt, 16000, draft) };
          }
          keys(branch, ['kind', 'model', 'prompt']);
          if (branch.kind !== 'model') throw new Error('Parallel branches must use an agent or model.');
          return { kind: 'model' as const, model: text(branch.model, 200, draft), prompt: text(branch.prompt, 16000, draft) };
        });
        return { id: name, kind: 'parallel', branches, next: edge(node.next) };
      }
      case 'agent':
        keys(node, ['id', 'kind', 'agent', 'prompt', 'next']);
        if (typeof node.agent !== 'string' || node.agent.length > 200 || (!draft && !agents.has(node.agent))) throw new Error('Workflow agent not found.');
        return { id: name, kind: node.kind, agent: node.agent, prompt: text(node.prompt, 16000, draft), next: edge(node.next) };
      case 'model':
        keys(node, ['id', 'kind', 'model', 'prompt', 'next']);
        return { id: name, kind: node.kind, model: text(node.model, 200, draft), prompt: text(node.prompt, 16000, draft), next: edge(node.next) };
      case 'condition':
        if (node.operator === 'model') {
          keys(node, ['id', 'kind', 'operator', 'model', 'value', 'yes', 'no']);
          return { id: name, kind: node.kind, operator: 'model', model: text(node.model, 200, draft), value: text(node.value, 16000, draft), yes: edge(node.yes), no: edge(node.no) };
        }
        keys(node, ['id', 'kind', 'operator', 'value', 'yes', 'no']);
        if (node.operator !== 'equals' && node.operator !== 'contains') throw new Error('Invalid workflow condition operator.');
        return { id: name, kind: node.kind, operator: node.operator, value: text(node.value, 16000, true), yes: edge(node.yes), no: edge(node.no) };
      case 'approval':
        keys(node, ['id', 'kind', 'prompt', 'yes', 'no']);
        // ponytail: accept the old rejection-edge field on read/save; remove after graph migration.
        return { id: name, kind: node.kind, prompt: text(node.prompt, 4000, draft), yes: edge(node.yes), ...(node.no === undefined ? {} : { no: edge(node.no) }) };
      case 'wait':
        keys(node, ['id', 'kind', 'event', 'timeoutSeconds', 'next']);
        if (typeof node.timeoutSeconds !== 'number' || !Number.isFinite(node.timeoutSeconds) || Math.abs(node.timeoutSeconds) > Number.MAX_SAFE_INTEGER || (!draft && (!Number.isSafeInteger(node.timeoutSeconds) || node.timeoutSeconds < 1 || node.timeoutSeconds > 604800))) throw new Error('Event wait time must be 1 to 604800 seconds.');
        return { id: name, kind: node.kind, event: text(node.event, 256, draft), timeoutSeconds: Number(node.timeoutSeconds), next: edge(node.next) };
      case 'output':
        keys(node, ['id', 'kind', 'channel', 'discordChannel']);
        if (node.channel !== undefined && node.discordChannel !== undefined) throw new Error('Choose one output destination.');
        if (node.channel !== undefined && (typeof node.channel !== 'string' || (!draft && !/^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/.test(node.channel)) || node.channel.length > 36)) throw new Error('Invalid output channel.');
        return { id: name, kind: node.kind, ...(node.channel === undefined ? {} : { channel: node.channel }), ...(node.discordChannel === undefined ? {} : { discordChannel: discordChannel(node.discordChannel) }) };
      default: throw new Error('Unsupported workflow step type.');
    }
  });
  const known = new Set(nodes.map(node => node.id));
  if (known.size !== nodes.length || (!draft && (!known.has(entry) || nodes.some(node => workflowEdges(node).some(next => !known.has(next)))))) throw new Error('Duplicate workflow ID or link to a missing step.');
  const reachable = new Set([entry]), terminating = new Set(nodes.filter(node => node.kind === 'output').map(node => node.id));
  for (let pass = 0; pass < nodes.length; pass++) for (const node of nodes) {
    if (reachable.has(node.id)) for (const next of workflowEdges(node)) reachable.add(next);
    if (workflowEdges(node).some(next => terminating.has(next))) terminating.add(node.id);
  }
  if (!draft && (reachable.size !== nodes.length || terminating.size !== nodes.length)) throw new Error('Every step must be reachable from the start and have a path to the output.');
  let positions: WorkflowGraph['positions'];
  if (graph.positions !== undefined) positions = Object.fromEntries(Object.entries(object(graph.positions)).map(([name, raw]) => {
    const point = object(raw); keys(point, ['x', 'y']);
    if (!known.has(name) || typeof point.x !== 'number' || typeof point.y !== 'number' || !Number.isFinite(point.x) || !Number.isFinite(point.y)
      || point.x < 64 || point.y < 64 || point.x > 20000 || point.y > 20000) throw new Error('Invalid workflow step position.');
    return [name, { x: point.x, y: point.y }];
  }));
  return { entry, maxSteps: Number(maxSteps), nodes, ...(trigger ? { trigger } : {}), ...(positions ? { positions } : {}) };
}

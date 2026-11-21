import type { StreamPartial, StreamSegment } from '../../shared/turn-stream';

export type TurnActivityData = {
  artifacts?: { id: string; name: string; kind: string; url: string | null }[];
  started_at?: string | null;
  stopped_at?: string | null;
  budget_stopped?: number;
  dispatch?: { source_turn: string; target_turn: string; mode: 'steer' | 'restart' }[];
  calls: { seq: number; model: string; purpose: string; phase: string; status: string; created: string; finished: string | null }[];
  tools: ToolLog[];
  harness?: NativeToolLog[];
  stream?: StreamSegment[];
  fallback_model: string | null;
};

export function streamWork(turn: { partial?: StreamPartial | null; activity?: TurnActivityData }) {
  return (turn.activity?.stream ?? []).filter(segment => !segment.final && (segment.kind === 'tool' || !!segment.text && !(turn.partial?.seq === segment.seq && turn.partial.segment === segment.id)));
}

type ActivityTurn = { status: string; paused: number; created: string; activity?: TurnActivityData; questions?: { answer: string | null }[] };
export const liveTurn = (turn: ActivityTurn) => ['queued', 'running'].includes(turn.status) && !turn.paused && !turn.questions?.some(question => question.answer === null);
export const livePartial = (turn: { status: string; paused: number; partial?: { status: string; text: string } | null }) => turn.status === 'running' && !turn.paused && !!turn.partial && ['pending', 'received'].includes(turn.partial.status) && !!turn.partial.text;
export const turnPollDelay = (turns: { status: string }[]) => turns.some(turn => ['queued', 'running'].includes(turn.status)) ? 400 : 1200;

type PendingInputTurn = {
  computerWait?: unknown;
  questions?: { answer: string | null }[];
  approvals?: { state: string }[];
  harnessApprovals?: { kind: string; state: string }[];
};

export function isTurnWaitingForInput(turn: PendingInputTurn) {
  return !!turn.computerWait || !!turn.questions?.some(question => question.answer === null) || pendingApprovalSubject(turn) !== null;
}

export function pendingApprovalSubject(turn: PendingInputTurn) {
  if (turn.approvals?.some(approval => approval.state === 'approval')) return 'connected app action';
  const approval = turn.harnessApprovals?.find(approval => approval.state === 'approval');
  if (!approval) return null;
  switch (approval.kind) {
    case 'command': return 'shell command';
    case 'file': return 'file change';
    case 'network': return 'network access request';
    case 'email': return 'email draft';
    default: return 'tool request';
  }
}

export function activityLabel(turn: ActivityTurn, now: number) {
  if (!['done', 'cancelled'].includes(turn.status) && turn.questions?.some(question => question.answer === null)) return 'Waiting for your answer';
  if (!['done', 'cancelled'].includes(turn.status) && turn.paused) return 'Paused';
  const calls = turn.activity?.calls ?? [];
  const start = turn.activity?.started_at ?? calls[0]?.created ?? (turn.status === 'cancelled' ? null : turn.created);
  const end = turn.status === 'cancelled' ? turn.activity?.stopped_at : liveTurn(turn) ? now : calls.at(-1)?.finished;
  const elapsed = start && end ? Math.max(0, Math.floor((new Date(end).getTime() - Date.parse(start)) / 1000)) : null;
  const duration = elapsed !== null && Number.isFinite(elapsed) ? elapsed >= 60 ? `${Math.floor(elapsed / 60)}m ${elapsed % 60}s` : elapsed === 0 ? '<1s' : `${elapsed}s` : null;
  if (turn.status === 'done') return duration ? `Worked for ${duration}` : 'Worked';
  if (turn.status === 'failed') return duration ? `Failed after ${duration}` : 'Failed';
  if (turn.status === 'interrupted') return duration ? `Interrupted after ${duration}` : 'Interrupted';
  if (turn.status === 'cancelled') return duration ? `Stopped after ${duration}` : 'Stopped';
  if (turn.status === 'queued') return 'Queued';
  return duration ? `Working… ${duration}` : 'Working…';
}

export function matchOffsets(text: string, query: string) {
  if (!query.trim()) return [];
  return [...text.matchAll(new RegExp(query.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), 'giu'))]
    .map(match => ({ start: match.index, end: match.index + match[0].length }));
}

export function toolTitle(name: string) {
  return name.split('__').at(-1)!.replace(/[_-]+/g, ' ').replace(/^./, char => char.toUpperCase());
}

type ToolLog = { id: string; name: string; title?: string | null; state?: string; failed?: number; after_action?: number };
type NativeToolLog = { id: string; name: string; state: string; failed?: number };

const hiddenTool = (name: string) => /^(?:user[ _-]?message|agent[ _-]?message|assistant[ _-]?message|reasoning|plan|context[ _-]?compaction|verification[ _-]?reminder)$/i.test(name);
const journalName = (name: string) => name.replace(/^mcp__minarbot__/, '');

export function stepTitle(name: string) {
  if (/^\//.test(name) || ['commandExecution', 'exec_command', 'Bash', 'bash'].includes(name)) return 'Run shell';
  if (['fileChange', 'apply_patch', 'Edit', 'MultiEdit'].includes(name)) return 'Edit files';
  const titles: Record<string, string> = { WebSearch: 'Search web', webSearch: 'Search web', imageView: 'View image', imageGeneration: 'Generate image', collabAgentToolCall: 'Coordinate with agent', dynamicToolCall: 'Call tool', mcpToolCall: 'Call tool' };
  return titles[name] ?? toolTitle(name.replace(/([a-z])([A-Z])/g, '$1 $2'));
}

export function stepState(state: string | undefined, turnStatus?: string) {
  if (['completed', 'done', 'received', 'succeeded'].includes(state ?? '')) return 'Completed';
  if (['failed', 'error', 'uncertain'].includes(state ?? '')) return 'Failed';
  if (['declined', 'denied', 'rejected'].includes(state ?? '')) return 'Declined';
  if (['waiting', 'waiting_for_user', 'approval', 'paused'].includes(state ?? '')) return 'Waiting';
  if (['interrupted', 'aborted'].includes(state ?? '')) return 'Interrupted';
  if (['cancelled', 'canceled', 'closed'].includes(state ?? '')) return 'Stopped';
  if (turnStatus === 'failed') return 'Failed';
  if (turnStatus === 'interrupted') return 'Interrupted';
  if (turnStatus === 'cancelled') return 'Stopped';
  return ['running', 'in_progress', 'inProgress'].includes(state ?? '') ? 'Running' : 'Pending';
}

export function mergeRunSteps<T extends ToolLog, N extends NativeToolLog>(tools: readonly T[], harness: readonly N[]) {
  const journal = new Map(tools.filter(tool => !hiddenTool(tool.name)).map(tool => [tool.id, tool]));
  const native = [...new Map(harness.filter(step => !hiddenTool(step.name) && step.id !== 'verification-reminder').map(step => [step.id, step])).values()];
  const nativeIds = new Set(native.map(step => step.id));
  const journalByName = new Map<string, T[]>();
  for (const tool of journal.values()) {
    if (nativeIds.has(tool.id)) continue;
    const name = journalName(tool.name), matches = journalByName.get(name) ?? [];
    matches.push(tool);
    journalByName.set(name, matches);
  }
  const steps = native.map(step => {
    // ponytail: legacy harness and journal IDs differ. Match their occurrences in order until runtimes expose a shared ID.
    const tool = journal.get(step.id) ?? journalByName.get(journalName(step.name))?.shift();
    if (tool) journal.delete(tool.id);
    return { id: step.id, name: step.name, title: stepTitle(step.name), tool, native: step };
  });
  return [...steps, ...[...journal.values()].map(tool => ({ id: tool.id, name: tool.name, title: tool.title ?? stepTitle(tool.name), tool, native: undefined }))];
}

type WorkTurn = {
  partial?: StreamPartial | null;
  room?: { quiet: { name: string; silent: string }[] } | null;
  status: string;
  routing?: string | null;
  recipient?: string | null;
  agent_name?: string;
  coordination?: { kind: string; child: string | null; owner_name: string; target_name: string }[];
  agentMessages?: { id: string; name: string; status: string; settled: number; expects_reply: number; taskId?: string; task?: string; wait?: boolean; agent?: string; thread?: string }[];
  receivedAgentReplies?: { id: string; name: string; status: string }[];
  questions?: { id: string; prompt: string; answer: string | null }[];
  activity?: TurnActivityData & { checkpoints?: { turn: string; state: string }[] };
};

type WorkRow = { id: string; text: string; prose?: boolean; task?: string | null; stepId?: string; state?: string };

export function turnActivitySteps(turn: WorkTurn, agentName: string): WorkRow[] {
  const rows: WorkRow[] = [];
  const stream = streamWork(turn);
  const streamTools: NativeToolLog[] = stream.filter(segment => segment.kind === 'tool' && segment.id.startsWith('tool-'))
    .map(segment => ({ id: segment.id.slice(5), name: segment.text, state: segment.state }));
  const journal = turn.activity?.tools ?? [];
  const toolIds = new Set(journal.map(tool => tool.id));
  const merged = mergeRunSteps(journal, [...streamTools.filter(step => !toolIds.has(step.id)), ...(turn.activity?.harness ?? [])]);
  const byId = new Map(merged.map(step => [step.id, step]));
  const shown = new Set<string>();
  const show = (id: string) => {
    const step = byId.get(id);
    if (!step || shown.has(id)) return;
    shown.add(id);
    rows.push({ id: `tool-${id}`, text: step.title, stepId: id, state: stepState(step.tool?.failed || step.native?.failed ? 'failed' : step.native?.state ?? step.tool?.state ?? 'completed', turn.status) });
  };
  for (const segment of stream) {
    if (segment.kind === 'text') rows.push({ id: `${segment.seq}:${segment.id}`, text: segment.text, prose: true });
    else if (segment.id.startsWith('tool-')) show(segment.id.slice(5));
  }
  for (const item of turn.room?.quiet ?? []) rows.push({ id: `quiet-${item.name}`, text: `${item.name} stayed quiet${item.silent === 'not_mentioned' ? ' (not mentioned)' : ''}` });
  if (!turn.room && (turn.routing || turn.recipient)) rows.push({ id: 'routing', text: turn.routing === 'pending' ? 'Selecting recipient' : turn.routing === 'clarify' ? 'Requested routing clarification' : turn.routing === 'none' ? 'No task needed' : turn.routing === 'released' ? 'Waiting for an available recipient' : `Recipient selected: ${turn.agent_name ?? agentName}` });
  for (const step of merged) if (step.native) show(step.id);
  for (const link of turn.activity?.dispatch ?? []) if (link.mode === 'steer') rows.push({ id: `dispatch-${link.target_turn}`, text: 'Guidance linked to running turn', task: link.source_turn });
  const actions = turn.coordination ?? [];
  for (let index = 0; index <= actions.length; index++) {
    for (const step of merged) if (Math.min(step.tool?.after_action ?? 0, actions.length) === index) show(step.id);
    const action = actions[index];
    if (action) rows.push({ id: `coordination-${index}`, text: `${action.owner_name} ${action.kind === 'delegate_task' ? 'delegated to' : 'transferred to'} ${action.target_name}`, task: action.child });
  }
  for (const item of turn.agentMessages ?? []) rows.push({ id: `message-${item.id}`, text: item.taskId ? `Delegated to ${item.name}: ${item.task?.slice(0, 160) ?? 'Task'}${(item.task?.length ?? 0) > 160 ? '…' : ''}` : `Message to ${item.name} · ${item.settled ? item.status : item.expects_reply ? 'Waiting for reply' : 'No reply requested'}`, task: item.taskId ?? null });
  for (const item of turn.receivedAgentReplies ?? []) rows.push({ id: `received-${item.id}`, text: `Result received from ${item.name} · ${item.status}` });
  for (const question of turn.questions ?? []) rows.push({ id: `question-${question.id}`, text: `${question.answer === null ? 'Asked' : 'Answered'}: ${question.prompt}` });
  for (const checkpoint of turn.activity?.checkpoints ?? []) rows.push({ id: `checkpoint-${checkpoint.turn}`, text: checkpoint.state === 'done' ? 'Git checkpoint captured' : `Git checkpoint ${checkpoint.state}` });
  if (turn.activity?.budget_stopped) rows.push({ id: 'budget', text: `Stopped after ${turn.activity.budget_stopped} tool steps` });
  if (turn.activity?.fallback_model) rows.push({ id: 'fallback', text: `Fallback model used: ${turn.activity.fallback_model}` });
  return rows;
}

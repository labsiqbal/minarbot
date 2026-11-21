import { usePolling } from './hooks/usePolling';
import { Markdown } from './Markdown';
import { EventConnection } from './EventConnection';
import { useEffect, useRef, useState, type ReactNode } from 'react';
import { WorkflowEditor, type Workflow, type WorkflowDraft } from './WorkflowEditor';
import { WorkflowSchedule } from './WorkflowSchedule';
import { WorkflowWebhook, type WorkflowHook } from './WorkflowWebhook';
import type { WorkflowGraph } from './workflows';
import { WorkflowGraph as GraphView } from './WorkflowGraph';
import { restoreWorkflowEditorDraft, workflowDraftMatches, workflowStatusLabels as statuses, workflowStepLabel, workflowNodeLabels } from './workflow-editor-state';
import { restoreWorkflowCommand, type WorkflowCommand as Command } from './workflow-command';
import { ActionMenu, Alert, Badge, Button, Card, CodeBlock, Collapsible, EmptyState, Field, IconButton, PageHeader, Row, Section, SkeletonRows, Textarea, useConfirm, RecoveryAlert } from './ui/kit';
import { ChevronLeft, ChevronRight, Ellipsis, RefreshCw } from 'lucide-react';

type Agent = { id: string; name: string; model: string };
type Run = { id: string; version: number; revision: number; status: string; cursor: string; error: string | null; last: string; graph: WorkflowGraph; steps: { parallel_group: number | null; parallel_branch: number | null; seq: number; node: string; status: string; detail: string | null; task_status: string | null; paused: number | null; thread: string | null; agent: string | null }[] };

function stepTitle(run: Run, step: Run['steps'][number]) {
  const node = run.graph.nodes.find(node => node.id === step.node);
  if (node?.kind !== 'parallel') return step.node;
  return step.parallel_group === null ? `${step.node} · Merge` : `${step.node} · Branch ${step.parallel_branch ?? step.seq - step.parallel_group + node.branches.length + 1}`;
}

function WorkflowText({ text, run }: { text: string; run: string }) {
  return <div className="legacy" style={{ minWidth: 0, maxWidth: '100%', overflowX: 'auto' }}><Markdown text={text} scope={`/api/workflow-runs/${run}`} origin={window.location.origin} /></div>;
}

function WorkflowVersions({ workflow, request, locked, refresh, onRestore }: {
  workflow: Workflow; request: <T>(path: string, data?: unknown) => Promise<T>; locked: boolean; refresh: number; onRestore: (version: number) => void;
}) {
  const [cursors, setCursors] = useState<number[]>([]), [error, setError] = useState(''), [retry, setRetry] = useState(0);
  const [page, setPage] = useState<{ versions: { revision: number; graph: WorkflowGraph }[]; next_before: number | null } | null>(null);
  const confirm = useConfirm();
  const before = cursors.at(-1);
  useEffect(() => {
    let stopped = false; setPage(null); setError('');
    void request<NonNullable<typeof page>>(`/api/workflows/${workflow.id}/versions${before ? `?before=${before}` : ''}`).then(data => { if (!stopped) setPage(data); }).catch(error => { if (!stopped) setError(error.message); });
    return () => { stopped = true; };
  }, [workflow.id, workflow.revision, before, refresh, retry]);
  return <Section title="Saved versions">
    <p className="k-field__hint">Restore the graph as a new revision. The workflow name and runs already started stay unchanged.</p>
    {error ? <Alert tone="error" actions={<Button size="sm" onClick={() => setRetry(value => value + 1)}>Reload versions</Button>}>{error}</Alert> : !page ? <><span className="sr-only" role="status">Loading versions...</span><SkeletonRows /></> : <Card>{page.versions.map(version => <Row key={version.revision} title={`Version ${version.revision}`} meta={<><span>{version.graph.nodes.length} steps</span><span>Start: {version.graph.entry}</span>{version.revision === workflow.revision && <Badge>Current</Badge>}</>}
      trailing={<ActionMenu label={`Version ${version.revision} actions`} trigger={<IconButton label={`Version ${version.revision} actions`}><Ellipsis size={16} aria-hidden /></IconButton>} items={[{ label: `Restore version ${version.revision}`, disabled: locked || version.revision === workflow.revision, onSelect: async () => { if (await confirm({ title: `Restore graph version ${version.revision} as a new revision?`, description: 'Runs already started stay unchanged.', confirm: 'Restore version' })) onRestore(version.revision); } }]} />}>
      <Collapsible title={<>View graph version {version.revision}</>}><CodeBlock label={`Graph version ${version.revision}`}>{JSON.stringify(version.graph, null, 2)}</CodeBlock></Collapsible>
    </Row>)}</Card>}
    {(cursors.length > 0 || page?.next_before) && <div className="k-pager"><IconButton label="Newer versions" disabled={!cursors.length} onClick={() => setCursors(value => value.slice(0, -1))}><ChevronLeft size={16} aria-hidden /></IconButton><IconButton label="Older versions" disabled={!page?.next_before} onClick={() => { if (page?.next_before) setCursors(value => [...value, page.next_before!]); }}><ChevronRight size={16} aria-hidden /></IconButton></div>}
  </Section>;
}

function RunGraph({ run }: { run: Run }) {
  const [node, setNode] = useState('');
  const buttons = useRef(new Map<string, HTMLButtonElement>()), detail = useRef<HTMLDivElement>(null);
  const steps = run.steps.filter(step => step.node === node), selected = run.graph.nodes.find(item => item.id === node);
  useEffect(() => { if (node) detail.current?.focus(); }, [node]);
  return <section><GraphView graph={run.graph} selected={node || run.cursor} locked={false} labels={workflowNodeLabels(run.steps)} buttonRefs={buttons.current} onSelect={id => { setNode(id); if (id === node) detail.current?.focus(); }} />
    {selected && <div className="workflow-step-detail" tabIndex={-1} ref={detail}><h3>Step {node}</h3>{steps.length ? <ol>{steps.map((step, index) => <li key={step.seq}>Attempt {index + 1}: {workflowStepLabel(step)}{step.detail && <WorkflowText text={step.detail} run={run.id} />}</li>)}</ol> : <p>Step has not run in this execution yet.</p>}<Button onClick={() => { buttons.current.get(node)?.focus(); buttons.current.get(node)?.scrollIntoView({ block: 'nearest', inline: 'nearest' }); }}>Back to run graph</Button></div>}
  </section>;
}

export function WorkflowRunDetails({ id, request, onThread }: {
  id: string; request: <T>(path: string, data?: unknown, signal?: AbortSignal) => Promise<T>;
  onThread: (agent: string, thread: string) => void;
}) {
  const [run, setRun] = useState<Run>();
  const [error, setError] = useState('');
  usePolling(async signal => {
    try {
      const result = await request<Run>('/api/workflow-runs/' + id, undefined, signal);
      if (!signal.aborted) { setRun(result); setError(''); }
    } catch (reason) { if (!signal.aborted) setError(reason instanceof Error ? reason.message : 'Run could not load.'); }
  }, { interval: 1200 }, [id, request]);
  return <div className="k-stack">
    {error && <Alert tone="error">{error}</Alert>}
    {!run && !error && <SkeletonRows />}
    {run && <>
      <div><Badge>Version {run.version} · {statuses[run.status] ?? run.status}</Badge></div>
      {run.error && <Alert tone="error">{run.error}</Alert>}
      <RunGraph key={run.id} run={run} />
      <WorkflowText text={run.last} run={run.id} />
      <Card>{run.steps.map(step => <Row key={step.seq} title={step.node} meta={<Badge>{workflowStepLabel(step)}</Badge>}
        trailing={step.thread && step.agent && <Button size="sm" onClick={() => onThread(step.agent!, step.thread!)}>Open step {step.node}</Button>} />)}</Card>
    </>}
  </div>;
}

export function WorkflowPanel({ owner, agents, request, onBack, renderConversation, onNavigate, initial }: {
  owner: Agent; agents: Agent[]; request: <T>(path: string, data?: unknown) => Promise<T>;
  onBack: () => void; renderConversation: (thread: string, owner: string) => ReactNode; onNavigate: () => void;
  initial?: Workflow | 'new';
}) {
  const storage = `minarbot:workflow-command:${owner.id}`, listPath = `/api/agents/${owner.id}/workflows`;
  const [recovery, setRecovery] = useState(() => {
    try { return { command: restoreWorkflowCommand(sessionStorage.getItem(storage), owner.id, new Set(agents.map(agent => agent.id))), invalid: false }; }
    catch { return { command: null, invalid: true }; }
  });
  const [pending, setPending] = useState<Command | null>(recovery.command);
  const [page, setPage] = useState<{ workflows: Workflow[]; next_after: string | null } | null>(null), [cursors, setCursors] = useState(['']);
  const [editing, setEditing] = useState<Workflow | 'new' | null>(pending?.kind === 'save' ? pending.workflow ?? 'new' : initial === 'new' && !pending ? 'new' : null);
  const [selected, setSelected] = useState<Workflow | null>(pending?.workflow ?? (initial && initial !== 'new' ? initial : null)), [runs, setRuns] = useState<{ seq: number; id: string; status: string; created: string }[] | null>(null);
  const [runId, setRunId] = useState(pending?.kind === 'run' ? String(pending.body.id) : pending && ['approve', 'cancel', 'retry'].includes(pending.kind) ? pending.path.split('/')[3] : ''), [run, setRun] = useState<Run | null>(null), [input, setInput] = useState('');
  const [runCursors, setRunCursors] = useState<number[]>([]), beforeRun = runCursors.at(-1);
  const [conversation, setConversation] = useState<{ thread: string; agent: string; node: string } | null>(null);
  const stepButtons = useRef(new Map<number, HTMLButtonElement>()), returnStep = useRef<number | null>(null), conversationBack = useRef<HTMLButtonElement>(null);
  useEffect(() => { setRunCursors([]); }, [selected?.id]);
  useEffect(() => { setRuns(null); }, [selected?.id, beforeRun]);
  useEffect(() => {
    if (conversation) conversationBack.current?.focus();
    else if (returnStep.current !== null) stepButtons.current.get(returnStep.current)?.focus();
  }, [conversation]);
  const [error, setError] = useState(''), [busy, setBusy] = useState(false), [refresh, setRefresh] = useState(0);
  const [conflict, setConflict] = useState(false), [editorReload, setEditorReload] = useState(0);
  const [scheduleRefresh, setScheduleRefresh] = useState(0), [scheduleConflict, setScheduleConflict] = useState('');
  const [hookRefresh, setHookRefresh] = useState(0), [hookConflict, setHookConflict] = useState('');
  const [hookSecret, setHookSecret] = useState<{ workflow: string; value: string } | null>(null);
  const mounted = useRef(true), inFlight = useRef(false), navigation = useRef(0), heading = useRef<HTMLHeadingElement>(null);
  function navigate() { navigation.current++; setHookSecret(null); onNavigate(); }
  useEffect(() => () => { mounted.current = false; navigation.current++; }, []);
  const after = cursors[cursors.length - 1];
  useEffect(() => {
    let stopped = false; setPage(null);
    void Promise.all([
      request<{ workflows: Workflow[]; next_after: string | null }>(`${listPath}${after ? `?after=${after}` : ''}`),
      selected ? request<Workflow>(`/api/workflows/${selected.id}`) : Promise.resolve(null),
    ]).then(([data, current]) => { if (!stopped) { setPage(data); if (current) setSelected(current); setError(''); } }).catch(error => { if (!stopped) setError(String(error.message)); });
    return () => { stopped = true; };
  }, [after, refresh, listPath, selected?.id]);
  useEffect(() => { heading.current?.focus(); }, [editing, selected?.id]);
  usePolling(async signal => {
    try {
      const rows = await request<NonNullable<typeof runs>>(`/api/workflows/${selected!.id}/runs${beforeRun ? `?before=${beforeRun}` : ''}`);
      const detail = runId ? await request<Run>(`/api/workflow-runs/${runId}`) : null;
      if (!signal.aborted) { setRuns(rows); setRun(detail); }
    } catch (error) { if (!signal.aborted) setError(error instanceof Error ? error.message : 'History has not loaded.'); }
  }, { interval: 1200, enabled: !!selected }, [selected?.id, runId, refresh, beforeRun]);
  async function execute(command: Command) {
    if (busy || inFlight.current) return false;
    const version = navigation.current;
    try { sessionStorage.setItem(storage, JSON.stringify(command)); }
    catch { setError('Request has not been saved in the browser. Check site storage.'); return false; }
    inFlight.current = true; setPending(command); setBusy(true); setError('');
    if (command.kind === 'webhook') setHookSecret(null);
    try {
      const result = await request<{ id: string } & Partial<WorkflowHook>>(command.path, command.body);
      if (JSON.parse(sessionStorage.getItem(storage) ?? 'null')?.body?.id === command.body.id) sessionStorage.removeItem(storage);
      if (command.kind === 'save') {
        const key = `minarbot:workflow-draft:${owner.id}:${command.workflow?.id ?? 'new'}`;
        try {
          const draft = restoreWorkflowEditorDraft(sessionStorage.getItem(key));
          if (draft && draft.revision === command.body.revision && workflowDraftMatches(draft.name, draft.graph, command.body as unknown as WorkflowDraft, new Set(agents.map(agent => agent.id)))) sessionStorage.removeItem(key);
        } catch { /* Preserve an unreadable or newer local draft. */ }
      }
      if (!mounted.current) return false;
      setPending(null); setRefresh(value => value + 1);
      if (command.kind === 'schedule' && command.workflow?.id === selected?.id && version === navigation.current) setScheduleRefresh(value => value + 1);
      if (version === navigation.current) {
        if (command.kind === 'delete') { setSelected(null); setRunId(''); setRun(null); if (initial) onBack(); }
        if (command.kind === 'webhook' && command.workflow?.id === selected?.id) {
          setHookRefresh(value => value + 1);
          if (result.secret) setHookSecret({ workflow: selected!.id, value: result.secret });
        }
        if (command.kind === 'save') { setEditing(command.autosave ? result as Workflow : null); if (command.workflow?.id === selected?.id || (initial && !command.workflow)) setSelected(result as Workflow); }
        if (command.kind === 'run' && command.workflow?.id === selected?.id) { setRun(null); setRunId(result.id); }
        if (command.kind === 'restore' && command.workflow?.id === selected?.id) setSelected(result as Workflow);
      }
      return true;
    } catch (error) {
      if (!mounted.current) return false;
      if (typeof error === 'object' && error && 'status' in error && Number(error.status) < 500) {
        if (JSON.parse(sessionStorage.getItem(storage) ?? 'null')?.body?.id === command.body.id) sessionStorage.removeItem(storage); setPending(null);
        if (command.kind === 'save' && Number(error.status) === 409 && version === navigation.current) setConflict(true);
        if (command.kind === 'webhook' && Number(error.status) === 409) setHookConflict(command.workflow!.id);
        if (command.kind === 'schedule' && Number(error.status) === 409) setScheduleConflict(command.workflow!.id);
      }
      setError(error instanceof Error ? error.message : 'Request is not confirmed yet.');
      return false;
    } finally { inFlight.current = false; if (mounted.current) setBusy(false); }
  }
  async function reloadEdit() {
    if (!editing || editing === 'new' || busy) return;
    const version = navigation.current; setBusy(true);
    try {
      const current = await request<Workflow>(`/api/workflows/${editing.id}`);
      if (mounted.current && version === navigation.current) { setEditing(current); setEditorReload(value => value + 1); setConflict(false); setError(''); }
    } catch (error) { if (mounted.current && version === navigation.current) setError(error instanceof Error ? error.message : 'Workflow has not loaded.'); }
    finally { if (mounted.current) setBusy(false); }
  }
  const confirm = useConfirm();
  const locked = busy || !!pending || conflict || recovery.invalid || !!selected?.deleted || !!(editing && editing !== 'new' && editing.deleted);
  const banner = <>{editing && editing !== 'new' && !!editing.deleted && <Alert tone="warn">Workflow has been deleted. The draft remains saved in the browser.</Alert>}{recovery.invalid && <Alert tone="error" actions={<Button size="sm" onClick={() => { try { sessionStorage.removeItem(storage); setRecovery({ command: null, invalid: false }); } catch { setError('Site storage is unavailable.'); } }}>Ignore recovery data</Button>}>Saved request could not be read. Check history before resending.</Alert>}{error && <Alert tone="error">{error}</Alert>}{pending && <RecoveryAlert inFlight={busy} tone="warn" title="Request is not confirmed yet" actions={<Button size="sm" disabled={busy} onClick={() => void execute(pending)}>Retry request</Button>} />}{conflict && editing && <Alert tone="warn" actions={<Button size="sm" disabled={busy} onClick={() => void reloadEdit()}>Load latest version</Button>}>Draft remains visible. Load the latest version; previous graph changes can be restored with Undo graph.</Alert>}</>;
  if (conversation && selected && run) return <><div className="k-page bs-root"><PageHeader title={selected.name} description={`${selected.name} · Version ${run.version} · Step ${conversation.node} · Run ${run.id}`} actions={<Button ref={conversationBack} variant="ghost" onClick={() => setConversation(null)}>Back to run {selected.name}</Button>} /></div>{renderConversation(conversation.thread, conversation.agent)}</>;
  if (editing) return <><div className="k-page bs-root">{banner}</div><WorkflowEditor key={editing === 'new' ? 'new' : editing.id} reload={editorReload} owner={owner} agents={agents} request={request} locked={locked} saved={editing === 'new' ? undefined : editing} draftKey={`minarbot:workflow-draft:${owner.id}:${editing === 'new' ? 'new' : editing.id}`} pendingSave={pending?.kind === 'save'} onReload={() => void reloadEdit()}
    initial={pending?.kind === 'save' ? pending.body as unknown as WorkflowDraft : editing === 'new' ? undefined : editing}
    onBack={() => { navigate(); if (initial && !selected) onBack(); else { setEditing(null); setConflict(false); setRefresh(value => value + 1); } }}
    onSave={(draft, automatic) => execute({ kind: 'save', ...(automatic ? { autosave: true } : {}), workflow: editing === 'new' ? undefined : editing, path: editing === 'new' ? listPath : `/api/workflows/${editing.id}/edit`, body: { id: crypto.randomUUID(), ...draft } })} /></>;
  return <div className="k-page bs-root">
    <PageHeader headingRef={heading} title={selected ? selected.name : 'Workflows'} actions={<>
      <Button variant="ghost" onClick={() => { navigate(); if (selected && !initial) { setSelected(null); setRunId(''); setRun(null); } else onBack(); }}>Back</Button>
      <IconButton label="Reload workflow" onClick={() => setRefresh(value => value + 1)}><RefreshCw size={15} aria-hidden /></IconButton>
      {selected && !selected.deleted && initial && <Button disabled={locked} onClick={() => { navigate(); setEditing(selected); }}>Edit workflow</Button>}
      {!selected && <Button variant="primary" disabled={locked} onClick={() => { navigate(); setEditing('new'); }}>Create workflow</Button>}
    </>} />{banner}
    {!selected ? <Section title="Workflow library">
      {!page ? <><span className="sr-only" role="status">Loading workflow...</span><SkeletonRows /></> : !page.workflows.length ? <EmptyState title="No workflows on this page yet" /> : <Card>{page.workflows.map(item => <Row key={item.id} title={item.name} meta={<><Badge>Version {item.revision}</Badge><span>{item.graph.nodes.length} steps</span></>} onClick={() => { navigate(); setSelected(item); setRunId(''); setRun(null); setRuns(null); setInput(''); }} trailing={<ActionMenu label={`${item.name} actions`} trigger={<IconButton label={`${item.name} actions`}><Ellipsis size={16} aria-hidden /></IconButton>} items={[{ label: `Edit ${item.name}`, disabled: locked, onSelect: () => { navigate(); setEditing(item); } }]} />} />)}</Card>}
      {(cursors.length > 1 || page?.next_after) && <div className="k-pager"><IconButton label="Previous workflows" disabled={cursors.length < 2} onClick={() => setCursors(value => value.slice(0, -1))}><ChevronLeft size={16} aria-hidden /></IconButton><IconButton label="Next workflows" disabled={!page?.next_after} onClick={() => { if (page?.next_after) setCursors(value => [...value, page.next_after!]); }}><ChevronRight size={16} aria-hidden /></IconButton></div>}
    </Section> : <>{selected.deleted ? <Alert tone="warn">Workflow has been deleted. Saved results can still be read.</Alert> : <Section title="Start a run" actions={<ActionMenu label="Workflow actions" trigger={<IconButton label="Workflow actions"><Ellipsis size={16} aria-hidden /></IconButton>} items={[{ label: 'Delete workflow', danger: true, disabled: locked, onSelect: async () => { if (await confirm({ title: `Delete workflow ${selected.name}?`, description: 'Schedules are stopped and the webhook URL is revoked. Saved results remain available. Active runs must be stopped first.', confirm: 'Delete workflow', danger: true })) void execute({ kind: 'delete', workflow: selected, path: `/api/workflows/${selected.id}/delete`, body: { id: crypto.randomUUID(), revision: selected.revision } }); } }]} />}>
      <form className="k-fieldset" onSubmit={event => { event.preventDefault(); if (!locked) void execute({ kind: 'run', workflow: selected, path: `/api/workflows/${selected.id}/run`, body: { id: crypto.randomUUID(), revision: selected.revision, input } }); }}><Field label="Workflow input"><Textarea maxLength={16000} value={input} readOnly={locked} onChange={event => setInput(event.target.value)} /></Field><div className="k-inline"><Button type="submit" variant="primary" disabled={locked}>Run version {selected.revision}</Button></div></form>
    </Section>}
      {!selected.deleted && <>
      <WorkflowSchedule key={`schedule:${selected.id}`} workflow={selected.id} request={request} locked={locked} refresh={scheduleRefresh} conflict={scheduleConflict === selected.id} onLoaded={() => setScheduleConflict(current => current === selected.id ? '' : current)} pending={pending?.kind === 'schedule' && pending.workflow?.id === selected.id ? pending : undefined} onSave={async body => { await execute({ kind: 'schedule', workflow: selected, path: `/api/workflows/${selected.id}/schedule`, body }); }} onRun={id => { navigate(); setRun(null); setRunId(id); }} />
      <WorkflowWebhook key={`hook:${selected.id}`} workflow={selected.id} request={request} locked={locked} busy={busy} refresh={hookRefresh} secret={hookSecret?.workflow === selected.id ? hookSecret.value : undefined} conflict={hookConflict === selected.id} onLoaded={() => setHookConflict(current => current === selected.id ? '' : current)} pending={pending?.kind === 'webhook' && pending.workflow?.id === selected.id ? pending : undefined} onSave={async body => { await execute({ kind: 'webhook', workflow: selected, path: `/api/workflows/${selected.id}/webhook`, body }); }} onRun={id => { navigate(); setRun(null); setRunId(id); }} />
      <WorkflowVersions key={selected.id} workflow={selected} request={request} locked={locked} refresh={refresh} onRestore={version => void execute({ kind: 'restore', workflow: selected, path: `/api/workflows/${selected.id}/restore`, body: { id: crypto.randomUUID(), revision: selected.revision, version } })} /></>}
      <Section title="Run history">{!runs ? <><span className="sr-only" role="status">Loading run history...</span><SkeletonRows /></> : !runs.length ? <EmptyState title="No runs on this page yet" /> : <Card>{runs.map(item => <Row key={item.id} title={new Date(item.created).toLocaleString('en-US')} meta={<Badge tone={item.status === 'failed' || item.status === 'approval' ? 'warn' : item.status === 'done' ? 'success' : 'neutral'}>{item.status === 'done' ? 'Done' : statuses[item.status] ?? item.status}</Badge>} trailing={<Button size="sm" aria-pressed={runId === item.id} onClick={() => { navigate(); setRun(null); setRunId(item.id); }}>Open run</Button>} />)}</Card>}
      {(runCursors.length > 0 || runs?.length === 20) && <div className="k-pager"><IconButton label="Newer runs" disabled={!runCursors.length} onClick={() => setRunCursors(value => value.slice(0, -1))}><ChevronLeft size={16} aria-hidden /></IconButton><IconButton label="Older runs" disabled={runs?.length !== 20} onClick={() => { if (runs?.length) setRunCursors(value => [...value, runs.at(-1)!.seq]); }}><ChevronRight size={16} aria-hidden /></IconButton></div>}
      </Section>
      {run && <Section title={<>Version {run.version} <Badge tone={run.status === 'failed' || run.status === 'approval' ? 'warn' : run.status === 'done' ? 'success' : 'neutral'}>{run.status === 'done' ? 'Done' : statuses[run.status]}</Badge></>}>{run.error && <Alert tone="error">{run.error}</Alert>}
        <RunGraph key={run.id} run={run} />
        {run.status === 'failed' && ['agent', 'model', 'condition'].includes(run.graph.nodes.find(node => node.id === run.cursor)?.kind ?? '') && run.steps.length < run.graph.maxSteps && <Button disabled={locked} onClick={async () => { if (await confirm({ title: 'Retry the failed step?', description: 'Previous effects may already have occurred. The latest node settings of the same type will be used; previous results remain saved.', confirm: 'Retry step' })) void execute({ kind: 'retry', workflow: selected, path: `/api/workflow-runs/${run.id}/retry`, body: { id: crypto.randomUUID(), revision: run.revision } }); }}>Retry failed step</Button>}
        {run.status === 'waiting' && run.graph.nodes.find(node => node.id === run.cursor)?.kind === 'wait' && <div className="legacy"><EventConnection key={`${run.id}:${run.steps.at(-1)?.seq}`} path={`/api/workflow-waits/${run.id}/${run.steps.at(-1)!.seq}`} request={request} /></div>}
        {run.status === 'approval' && <><WorkflowText text={run.graph.nodes.find(node => node.id === run.cursor)?.kind === 'approval' ? (run.graph.nodes.find(node => node.id === run.cursor) as { prompt: string }).prompt : ''} run={run.id} /><div className="k-inline">{[true, false].map(accepted => <Button key={String(accepted)} disabled={locked} onClick={() => void execute({ kind: 'approve', workflow: selected, path: `/api/workflow-runs/${run.id}/approve`, body: { id: crypto.randomUUID(), revision: run.revision, accepted } })}>{accepted ? 'Approve step' : 'Reject step'}</Button>)}</div></>}
        {!['done', 'failed', 'cancelled'].includes(run.status) && <ActionMenu label="Run actions" trigger={<IconButton label="Run actions"><Ellipsis size={16} aria-hidden /></IconButton>} items={[{ label: 'Cancel workflow', danger: true, disabled: locked, onSelect: async () => { if (await confirm({ title: 'Cancel the workflow and its active step?', description: 'Saved results remain available.', confirm: 'Cancel workflow', danger: true })) void execute({ kind: 'cancel', workflow: selected, path: `/api/workflow-runs/${run.id}/cancel`, body: { id: crypto.randomUUID(), revision: run.revision } }); } }]} />}
        <WorkflowText text={run.last} run={run.id} /><Card>{run.steps.map(step => <Row key={step.seq} title={step.node} meta={<Badge>{workflowStepLabel(step)}</Badge>} trailing={step.thread && step.agent && <Button ref={element => { if (element) stepButtons.current.set(step.seq, element); else stepButtons.current.delete(step.seq); }} size="sm" onClick={() => { navigate(); returnStep.current = step.seq; setConversation({ thread: step.thread!, agent: step.agent!, node: step.node }); }}>Open step {step.node}</Button>}>{step.detail && <WorkflowText text={step.detail} run={run.id} />}</Row>)}</Card>
      </Section>}
    </>}
  </div>;
}

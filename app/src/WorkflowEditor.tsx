import { useEffect, useRef, useState, type FormEvent, type SelectHTMLAttributes } from 'react';
import { WorkflowGraph as GraphView } from './WorkflowGraph';
import { ActionMenu, Alert, Button, Field, IconButton, Input, PageHeader, Select, Textarea, useConfirm, useField } from './ui/kit';
import { Ellipsis, Plus, Redo2, Undo2 } from 'lucide-react';
import { editWorkflowHistory, moveWorkflowNode, restoreWorkflowEditorDraft, workflowAutosaveDraft, workflowDraftMatches, workflowUnconditionalCycles, type WorkflowHistory } from './workflow-editor-state';
import { removeWorkflowNode, validateWorkflow, workflowStepTypeLabel, workflowStepTypes, type WorkflowGraph, type WorkflowNode } from './workflows.ts';

export type Workflow = { id: string; agent: string; name: string; revision: number; graph: WorkflowGraph; deleted?: number };
export type WorkflowDraft = { name: string; graph: WorkflowGraph; revision: number };
type Agent = { id: string; name: string; model: string };
const stepTypeOptions = Object.entries(workflowStepTypes).map(([value, label]) => ({ value, label }));

// ponytail: native validation until kit Select supports required and empty values.
function RequiredSelect(props: SelectHTMLAttributes<HTMLSelectElement>) {
  const field = useField();
  return <select id={field?.id} aria-describedby={field?.describedBy} className="k-input" {...props} />;
}

export function WorkflowEditor({ owner, agents, request, initial, saved, reload = 0, locked: parentLocked, draftKey, pendingSave, onReload, onSave, onBack }: {
  owner: Agent; agents: Agent[]; initial?: WorkflowDraft; saved?: WorkflowDraft; locked: boolean;
  request: <T>(path: string, data?: unknown) => Promise<T>;
  reload?: number;
  draftKey: string; pendingSave: boolean; onReload: () => void;
  onSave: (draft: WorkflowDraft, automatic?: boolean) => Promise<boolean>; onBack: () => void;
}) {
  const [channels, setChannels] = useState<{ id: string; name: string; archived: number; members: string[] }[] | null>(null);
  const [channelError, setChannelError] = useState(''), [channelRefresh, setChannelRefresh] = useState(0);
  useEffect(() => {
    let stopped = false; setChannelError('');
    void request<NonNullable<typeof channels>>('/api/channels').then(rows => { if (!stopped) setChannels(rows.filter(row => !row.archived && row.members.includes(owner.id))); }).catch(error => { if (!stopped) setChannelError(error.message); });
    return () => { stopped = true; };
  }, [owner.id, channelRefresh]);
  const [recovery, setRecovery] = useState(() => {
    try { return { draft: restoreWorkflowEditorDraft(sessionStorage.getItem(draftKey), saved?.graph), invalid: false, held: pendingSave }; }
    catch { return { draft: null, invalid: true, held: pendingSave }; }
  });
  const [draftConflict, setDraftConflict] = useState(!!recovery.draft && recovery.draft.revision !== (saved?.revision ?? 0));
  const locked = parentLocked || recovery.invalid || recovery.held || draftConflict;
  const [name, setName] = useState((!recovery.held ? recovery.draft?.name : undefined) ?? initial?.name ?? '');
  const [history, setHistory] = useState<WorkflowHistory>({ past: [], future: [], graph: (!recovery.held ? recovery.draft?.graph : undefined) ?? initial?.graph ?? { entry: 'work', maxSteps: 20, nodes: [
    { id: 'work', kind: 'agent', agent: owner.id, prompt: '', next: 'review' },
    { id: 'review', kind: 'approval', prompt: 'Approve the result to complete the workflow?', yes: 'output' },
    { id: 'output', kind: 'output' },
  ] } });
  const graph = history.graph, cycles = workflowUnconditionalCycles(history.graph);
  const [selected, setSelected] = useState(graph.entry);
  const panels = useRef(new Map<string, HTMLFieldSetElement>()), buttons = useRef(new Map<string, HTMLButtonElement>());
  const focusAfterRemoval = useRef('');
  function setGraph(change: WorkflowGraph | ((graph: WorkflowGraph) => WorkflowGraph)) {
    setHistory(current => editWorkflowHistory(current, typeof change === 'function' ? change(current.graph) : change));
  }
  const [error, setError] = useState('');
  const [storageError, setStorageError] = useState(false);
  const saveRef = useRef(onSave), attempted = useRef('');
  saveRef.current = onSave;
  async function submit(value: WorkflowDraft, automatic?: boolean) {
    let raw: string | null = null;
    try { raw = sessionStorage.getItem(draftKey); } catch { /* The command path reports unavailable storage. */ }
    const accepted = await saveRef.current(value, automatic);
    if (accepted) try { if (sessionStorage.getItem(draftKey) === raw) sessionStorage.removeItem(draftKey); } catch { /* Keep the recoverable draft if cleanup fails. */ }
    return accepted;
  }
  const draft = workflowAutosaveDraft(name, graph, saved, new Set(agents.map(agent => agent.id)));
  const stamp = draft ? JSON.stringify(draft) : '';
  useEffect(() => {
    if (locked || !stamp || attempted.current === stamp) return;
    const timer = setTimeout(() => {
      attempted.current = stamp; setError('');
      void submit(JSON.parse(stamp) as WorkflowDraft, true);
    }, 400);
    return () => clearTimeout(timer);
  }, [stamp, locked]);
  const dirty = saved ? !workflowDraftMatches(name, graph, saved, new Set(agents.map(agent => agent.id))) : !!recovery.draft || !!name || history.past.length > 0;
  useEffect(() => {
    if (pendingSave || recovery.held || recovery.invalid || draftConflict) return;
    try {
      if (dirty) sessionStorage.setItem(draftKey, JSON.stringify({ name, graph, revision: saved?.revision ?? 0 }));
      else sessionStorage.removeItem(draftKey);
      setStorageError(false);
    } catch { setStorageError(true); }
  }, [name, graph, saved?.revision, dirty, draftKey, recovery.invalid, recovery.held, pendingSave, draftConflict]);
  useEffect(() => {
    if (!dirty) return;
    const warn = (event: BeforeUnloadEvent) => { event.preventDefault(); };
    window.addEventListener('beforeunload', warn);
    return () => window.removeEventListener('beforeunload', warn);
  }, [dirty]);
  const confirm = useConfirm();
  const heading = useRef<HTMLHeadingElement>(null);
  const appliedReload = useRef(reload);
  useEffect(() => { heading.current?.focus(); }, []);
  useEffect(() => {
    if (appliedReload.current === reload || !saved) return;
    appliedReload.current = reload; attempted.current = '';
    setDraftConflict(false); setRecovery({ draft: null, invalid: false, held: false });
    setHistory(current => editWorkflowHistory(current, saved.graph)); setName(saved.name); setError('');
    heading.current?.focus();
  }, [reload, saved]);
  useEffect(() => {
    if (!focusAfterRemoval.current) return;
    const button = buttons.current.get(focusAfterRemoval.current);
    button?.focus(); button?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
    focusAfterRemoval.current = '';
  }, [graph]);
  function update(index: number, patch: Partial<WorkflowNode>) {
    setGraph(current => ({ ...current, nodes: current.nodes.map((node, i) => i === index ? { ...node, ...patch } as WorkflowNode : node) }));
  }
  function changeKind(index: number, kind: WorkflowNode['kind']) {
    const id = graph.nodes[index].id, next = graph.nodes.find(node => node.id !== id)?.id ?? id;
    const node: WorkflowNode = kind === 'output' ? { id, kind } : kind === 'approval' ? { id, kind, prompt: '', yes: next }
      : kind === 'parallel' ? { id, kind, branches: [{ kind: 'agent', agent: owner.id, prompt: '' }, { kind: 'agent', agent: owner.id, prompt: '' }], next }
      : kind === 'wait' ? { id, kind, event: '', timeoutSeconds: 3600, next }
      : kind === 'condition' ? { id, kind, operator: 'equals', value: '', yes: next, no: next }
        : kind === 'agent' ? { id, kind, agent: owner.id, prompt: '', next } : { id, kind, model: owner.model, prompt: '', next };
    setGraph(current => ({ ...current, nodes: current.nodes.map((item, i) => i === index ? node : item) }));
  }
  const targets = [<option key="" value="">Choose step</option>, ...graph.nodes.map(node => <option key={node.id} value={node.id}>{node.id}</option>)];
  async function save(event: FormEvent) {
    event.preventDefault(); if (locked) return;
    try {
      const validated = validateWorkflow(graph, new Set(agents.map(agent => agent.id)));
      setError(''); await submit({ name: name.trim(), graph: validated, revision: saved?.revision ?? initial?.revision ?? 0 });
    } catch (error) { setError(error instanceof Error ? error.message : 'Workflow has not been saved.'); }
  }
  return <div className="k-page bs-root">
    <PageHeader headingRef={heading} title={initial ? 'Edit workflow' : 'New workflow'} description="Arrange steps and their next destinations. Approvals wait for your decision. Saved changes apply only to new runs." actions={<><Button variant="ghost" onClick={async () => { if (!dirty || await confirm({ title: 'Leave editor?', description: 'There are unsaved changes. Leave the editor?', confirm: 'Leave', danger: true })) onBack(); }}>Back</Button><Button type="submit" form="workflow-editor" variant="primary" disabled={locked}>Save workflow</Button></>} />
    {recovery.held && !pendingSave && <Alert tone="warn">The request has completed. Review the local draft before continuing to edit. <Button type="button" disabled={parentLocked} onClick={() => {
      try {
        const local = restoreWorkflowEditorDraft(sessionStorage.getItem(draftKey), saved?.graph);
        setRecovery({ draft: local, invalid: false, held: false });
        setDraftConflict(!!local && local.revision !== (saved?.revision ?? 0));
        if (local) { setName(local.name); setHistory(current => editWorkflowHistory(current, local.graph)); }
      } catch { setRecovery({ draft: null, invalid: true, held: false }); }
    }}>Review local draft</Button></Alert>}
    {storageError && <Alert tone="error">Draft has not been saved in the browser. Do not close the page before saving to the server.</Alert>}
    {recovery.invalid && <Alert tone="error">Browser draft could not be read. <Button type="button" onClick={async () => { if (await confirm({ title: 'Discard the unreadable browser draft?', confirm: 'Discard draft', danger: true })) try { sessionStorage.removeItem(draftKey); setRecovery({ draft: null, invalid: false, held: false }); } catch { setStorageError(true); } }}>Discard browser draft</Button></Alert>}
    {draftConflict && <Alert tone="warn">Draft is from an older revision. Load the latest version before saving; the draft graph remains available through Undo graph. <Button type="button" disabled={parentLocked} onClick={onReload}>Load latest version</Button></Alert>}
    {saved && <p className="k-field__hint" role="status">{locked ? 'Saving awaits confirmation or recovery.' : !dirty ? `Saved, version ${saved.revision}.` : draft ? attempted.current === stamp ? 'Changes have not been saved. Use Save workflow to try again.' : 'Changes save automatically after you stop typing.' : 'Complete the graph and name to save changes.'}</p>}
    <form id="workflow-editor" className="k-fieldset" onSubmit={event => void save(event)}><fieldset className="k-fieldset" disabled={locked}>
      <Field label="Workflow name"><Input required maxLength={80} value={name} onChange={event => setName(event.target.value)} /></Field>
      <Field label="Trigger"><Select value={graph.trigger?.kind ?? 'manual'} disabled={locked} onChange={value => {
        const { trigger, ...base } = graph;
        setGraph(value === 'discord' ? { ...base, trigger: trigger ?? { kind: 'discord', channel: '' } } : base);
      }} options={[{ value: 'manual', label: 'Manual' }, { value: 'discord', label: 'Discord message' }]} /></Field>
      {graph.trigger && <Field label="Discord trigger channel ID"><Input required inputMode="numeric" pattern="[1-9][0-9]{0,19}" maxLength={20} value={graph.trigger.channel} onChange={event => setGraph({ ...graph, trigger: { kind: 'discord', channel: event.target.value } })} /></Field>}
      <Field label="Starting step"><RequiredSelect aria-label="Starting step" required value={graph.entry} onChange={event => setGraph({ ...graph, entry: event.target.value })}>{targets}</RequiredSelect></Field>
      <Field label="Step limit" hint="Loops stop with a failed status when this limit is reached."><Input type="number" min={1} max={1000} required value={graph.maxSteps} onChange={event => setGraph({ ...graph, maxSteps: Number(event.target.value) })} /></Field>
      <div className="k-inline"><Button type="button" disabled={!history.past.length} onClick={() => setHistory(current => editWorkflowHistory(current, 'undo'))} icon={<Undo2 size={14} aria-hidden />}>Undo graph</Button><Button type="button" disabled={!history.future.length} onClick={() => setHistory(current => editWorkflowHistory(current, 'redo'))} icon={<Redo2 size={14} aria-hidden />}>Redo graph</Button><Button type="button" disabled={!graph.positions} onClick={() => { const { positions, ...automatic } = graph; void positions; setGraph(automatic); }}>Auto-arrange</Button></div>
      <p className="k-field__hint">Choose a step in the graph to open its settings. Undo and redo apply to graph changes, not the workflow name.</p>
      {!!cycles.length && <Alert tone="warn">Unconditional loop: {cycles.map(cycle => cycle.join(', ')).join('; ')}. The loop stops if approval is denied, the run is canceled, or the step limit is reached.</Alert>}
      <GraphView graph={graph} loops={new Set(cycles.flat())} onMove={(id, x, y) => { setSelected(id); setGraph(current => moveWorkflowNode(current, id, x, y)); }} selected={selected} locked={locked} buttonRefs={buttons.current} onSelect={id => { setSelected(id); panels.current.get(id)?.focus(); panels.current.get(id)?.scrollIntoView({ block: 'nearest' }); }} />
      {graph.nodes.map((node, index) => { const unsupported = !Object.hasOwn(workflowStepTypes, node.kind); return <fieldset className="k-fieldset workflow-node" style={{ padding: 16, border: '0.5px solid var(--color-border-secondary)', borderRadius: 14 }} key={node.id} tabIndex={-1} ref={element => { if (element) panels.current.set(node.id, element); else panels.current.delete(node.id); }}><legend>Step {node.id}</legend>
        <div className="k-inline"><Button type="button" onClick={() => { setSelected(node.id); buttons.current.get(node.id)?.focus(); buttons.current.get(node.id)?.scrollIntoView({ block: 'nearest', inline: 'nearest' }); }}>Back to graph {node.id}</Button></div>
        <Field label="Step type" error={unsupported && 'Choose a supported type or delete this step.'}><Select disabled={locked} value={node.kind} valueLabel={unsupported ? workflowStepTypeLabel(node.kind) : undefined} onChange={value => changeKind(index, value as WorkflowNode['kind'])} options={stepTypeOptions} /></Field>
        {node.kind === 'agent' && <Field label="Executing agent" hint={agents.find(agent => agent.id === node.agent)?.name}><Select disabled={locked} value={node.agent} onChange={value => update(index, { agent: value })} options={agents.map(agent => ({ value: agent.id, label: agent.name.length > 24 ? agent.name.slice(0, 21) + '...' : agent.name }))} /></Field>}
        {node.kind === 'model' && <Field label="Model"><Input required maxLength={200} value={node.model} onChange={event => update(index, { model: event.target.value })} /></Field>}
        {node.kind === 'parallel' && <>
          <p className="k-field__hint">Branches receive the same input. Results merge in branch order when all finish. Agent queue and concurrency limits still apply. Failed blocks do not retry automatically; completed branch effects remain saved.</p>
          {node.branches.map((branch, position) => <fieldset className="k-fieldset" key={position}><legend>Branch {position + 1}</legend>
            <Field label="Executor"><Select disabled={locked} value={branch.kind} options={[{ value: 'agent', label: 'Agent' }, { value: 'model', label: 'Model' }]} onChange={value => update(index, { branches: node.branches.map((item, n) => n !== position ? item : value === 'agent' ? { kind: 'agent', agent: owner.id, prompt: item.prompt } : { kind: 'model', model: owner.model, prompt: item.prompt }) })} /></Field>
            {branch.kind === 'agent' ? <Field label="Branch agent" hint={agents.find(agent => agent.id === branch.agent)?.name}><Select disabled={locked} value={branch.agent} options={agents.map(agent => ({ value: agent.id, label: agent.name.length > 24 ? agent.name.slice(0, 21) + '...' : agent.name }))} onChange={value => update(index, { branches: node.branches.map((item, n) => n === position ? { ...branch, agent: value } : item) })} /></Field>
              : <Field label="Branch model"><Input required maxLength={200} value={branch.model} onChange={event => update(index, { branches: node.branches.map((item, n) => n === position ? { ...branch, model: event.target.value } : item) })} /></Field>}
            <Field label="Branch instructions"><Textarea required maxLength={16000} value={branch.prompt} onChange={event => update(index, { branches: node.branches.map((item, n) => n === position ? { ...item, prompt: event.target.value } : item) })} /></Field>
            <Button size="sm" disabled={locked || node.branches.length <= 2} onClick={() => { panels.current.get(node.id)?.focus(); update(index, { branches: node.branches.filter((_, n) => n !== position) }); }}>Remove branch {position + 1}</Button>
          </fieldset>)}
          <Button size="sm" disabled={locked || node.branches.length >= 8} onClick={() => update(index, { branches: [...node.branches, { kind: 'agent', agent: owner.id, prompt: '' }] })}>Add branch</Button>
        </>}
        {node.kind === 'wait' && <><Field label="Event name"><Input required maxLength={256} value={node.event} onChange={event => update(index, { event: event.target.value })} /></Field><Field label="Wait timeout (seconds)"><Input type="number" required min={1} max={604800} value={node.timeoutSeconds} onChange={event => update(index, { timeoutSeconds: Number(event.target.value) })} /></Field><p className="k-field__hint">Waits for a signed event for this run and step. The payload becomes the step result; a timeout fails the workflow.</p></>}
        {!unsupported && 'prompt' in node && <Field label={node.kind === 'approval' ? 'Approval question' : 'Step instructions'}><Textarea required maxLength={node.kind === 'approval' ? 4000 : 16000} rows={3} value={node.prompt} onChange={event => update(index, { prompt: event.target.value })} /></Field>}
        {node.kind === 'condition' && <><Field label="Condition evaluation"><Select disabled={locked} value={node.operator} onChange={value => {
          const operator = value as 'equals' | 'contains' | 'model';
          const base = { id: node.id, kind: 'condition' as const, value: node.value, yes: node.yes, no: node.no };
          const replacement: WorkflowNode = operator === 'model' ? { ...base, operator, model: owner.model } : { ...base, operator };
          setGraph(current => ({ ...current, nodes: current.nodes.map((item, i) => i === index ? replacement : item) }));
        }} options={[{ value: 'equals', label: 'Exact match' }, { value: 'contains', label: 'Contains text' }, { value: 'model', label: 'Model judgment' }]} /></Field>{node.operator === 'model' && <Field label="Condition model"><Input required maxLength={200} value={node.model} onChange={event => update(index, { model: event.target.value })} /></Field>}<Field label={node.operator === 'model' ? 'Condition question' : 'Comparison text'} hint={node.operator === 'model' ? 'The model answers yes or no without tools. Any other answer fails the step; the previous result is still passed on.' : 'Case-sensitive.'}><Input required={node.operator === 'model'} maxLength={16000} value={node.value} onChange={event => update(index, { value: event.target.value })} /></Field></>}
        {!unsupported && 'next' in node && <Field label="Continue to"><RequiredSelect aria-label="Continue to" required value={node.next} onChange={event => update(index, { next: event.target.value })}>{targets}</RequiredSelect></Field>}
        {(node.kind === 'approval' || node.kind === 'condition') && <><Field label={node.kind === 'approval' ? 'If approved' : 'If matched'}><RequiredSelect aria-label={node.kind === 'approval' ? 'If approved' : 'If matched'} required value={node.yes} onChange={event => update(index, { yes: event.target.value })}>{targets}</RequiredSelect></Field>{node.kind === 'condition' ? <Field label="If not matched"><RequiredSelect aria-label="If not matched" required value={node.no} onChange={event => update(index, { no: event.target.value })}>{targets}</RequiredSelect></Field> : <p className="k-field__hint">Rejection stops the run. Previous results remain saved.</p>}</>}
        {node.kind === 'output' && <><Field label="Result destination"><Select disabled={locked} value={node.discordChannel !== undefined ? 'discord' : node.channel === undefined ? 'inbox' : 'channel'} onChange={value => {
          setGraph(current => ({ ...current, nodes: current.nodes.map(item => item.id === node.id ? { id: node.id, kind: 'output', ...(value === 'channel' ? { channel: '' } : value === 'discord' ? { discordChannel: '' } : {}) } : item) }));
        }} options={[{ value: 'inbox', label: 'Workflow inbox' }, { value: 'channel', label: 'Section and inbox' }, { value: 'discord', label: 'Discord and inbox' }]} /></Field>
          {node.discordChannel !== undefined && <Field label="Discord output channel ID"><Input required inputMode="numeric" pattern="[1-9][0-9]{0,19}" maxLength={20} value={node.discordChannel} onChange={event => update(index, { discordChannel: event.target.value })} /></Field>}
          {node.channel !== undefined && <><Field label="Destination section" hint={channels?.find(channel => channel.id === node.channel)?.name}><RequiredSelect aria-label="Destination section" required value={node.channel} onChange={event => update(index, { channel: event.target.value })}><option value="">Choose section</option>{node.channel && !channels?.some(channel => channel.id === node.channel) && <option value={node.channel}>Section unavailable</option>}{channels?.map(channel => <option key={channel.id} value={channel.id}>{channel.name.length > 24 ? channel.name.slice(0, 21) + '...' : channel.name}</option>)}</RequiredSelect></Field>
            {channelError && <Alert tone="error">{channelError}</Alert>}{!channels && !channelError && <p role="status">Loading sections...</p>}<Button type="button" onClick={() => setChannelRefresh(value => value + 1)}>Reload destination sections</Button>
            <p className="k-field__hint">Results are sent on behalf of the workflow owner agent. The section must be active and the agent must still be a member when results are sent.</p></>}
          <p className="k-field__hint">Complete the workflow with the last step result.</p></>}
        <ActionMenu label={`Step ${node.id} actions`} trigger={<IconButton label={`Step ${node.id} actions`} disabled={locked}><Ellipsis size={16} aria-hidden /></IconButton>} items={[{ label: `Delete step ${node.id}`, danger: true, disabled: locked || graph.nodes.length === 1, onSelect: () => {
          const next = graph.nodes.find(item => item.id !== node.id)?.id;
          if (next) { focusAfterRemoval.current = next; setSelected(next); setGraph(current => removeWorkflowNode(current, node.id)); }
        } }]} />
      </fieldset>; })}
      <div className="k-inline"><Button type="button" disabled={graph.nodes.length >= 64} onClick={() => { let n = graph.nodes.length + 1; while (graph.nodes.some(node => node.id === `step${n}`)) n++; setGraph({ ...graph, nodes: [...graph.nodes, { id: `step${n}`, kind: 'output' }] }); }} icon={<Plus size={14} aria-hidden />}>Add step</Button></div>
    </fieldset>
    {error && <Alert tone="error">{error}</Alert>}
    </form>
  </div>;
}

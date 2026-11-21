import { lazy, Suspense, useId, useRef, useState, type FormEvent } from 'react';
import type { LocalProject } from '../shared/local-project';
import type { Spec } from '../shared/routine-schedule';
import { HttpError } from './http';
import { RoutineSchedule, scheduleLabel, type RoutineTiming } from './RoutineSchedule';
import { RunInspector } from './RunInspector';
import { useCalendarSlot, useCalendarSource, useCalendarWorkflows } from './hooks/useAutomationCalendar';
import { Alert, Badge, Button, Dialog, Field, Input, Select, SkeletonRows, Textarea, useConfirm } from './ui/kit';
import { calendarStatuses, dateLabel, moveRoutine, resolveCalendarTime, routineEdit, wallTime, workflowEdit,
  type CalendarAgent, type CalendarCommand, type CalendarItem, type CalendarKind, type CalendarRequest } from './automation-calendar';

const WorkflowRunDetails = lazy(() => import('./WorkflowPanel').then(module => ({ default: module.WorkflowRunDetails })));
type WallTime = { date: string; time: string };
export type CalendarSelection = { slot: WallTime } | { item: CalendarItem; target?: WallTime };
type Mode = 'details' | 'edit' | 'move' | 'run';
const message = (error: unknown) => error instanceof Error ? error.message : 'Calendar request failed.';

export function AutomationCalendarDialog({ selection, zone, agents, projects, defaultProject, defaultAgent, request, onClose, onSaved, onThread, onReport }: {
  selection: CalendarSelection; zone: string; agents: CalendarAgent[]; projects: LocalProject[];
  defaultProject: string; defaultAgent: string; request: CalendarRequest; onClose: () => void;
  onSaved: (ran: boolean) => void; onThread: (agent: string, thread: string) => void; onReport: (agent: string, destination: string, turn: string) => void;
}) {
  const item = 'item' in selection ? selection.item : undefined;
  const target = 'item' in selection ? selection.target : selection.slot;
  const itemAt = item?.at;
  const [mode, setMode] = useState<Mode>(item ? target ? 'move' : 'details' : 'edit');
  const [kind, setKind] = useState<CalendarKind>(item?.kind ?? 'routine');
  const [project, setProject] = useState(defaultProject);
  const [agent, setAgent] = useState(defaultAgent);
  const [workflowId, setWorkflowId] = useState('');
  const [reload, setReload] = useState(0);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [conflict, setConflict] = useState(false);
  const [pending, setPending] = useState<{ command: CalendarCommand; ran: boolean }>();
  const [timing, setTiming] = useState<RoutineTiming | null>(null);
  const [customMove, setCustomMove] = useState<{ spec: Spec; destination: WallTime }>();
  const applying = useRef(false);
  const confirm = useConfirm();
  const form = useId();
  const workflowKey = item?.kind === 'workflow' ? item.sourceId : workflowId;
  const routineKey = item?.kind === 'routine' ? item.sourceId : '';
  const slot = useCalendarSlot(request, item ? undefined : target?.date, item ? undefined : target?.time, zone);
  const { source, error: sourceError } = useCalendarSource(request, routineKey, workflowKey, item?.editable !== false, reload);
  const { workflows: choices, error: choicesError } = useCalendarWorkflows(request, !item && kind === 'workflow', project, reload);
  const workflows = choices?.filter(row => agents.some(entry => entry.id === row.agent && !entry.hidden));
  const projectRecord = projects.find(entry => entry.id === project);
  const availableAgents = agents.filter(entry => !entry.hidden && (!projectRecord || projectRecord.members.some(member => member.agent === entry.id)));
  const owner = availableAgents.some(entry => entry.id === agent) ? agent : availableAgents[0]?.id ?? '';
  const routine = source && 'routine' in source ? source.routine : undefined;
  const workflow = source && 'workflow' in source ? source.workflow : undefined;
  const schedule = source && 'schedule' in source ? source.schedule : null;
  const sourceRevision = routine?.revision ?? schedule?.revision;
  const enabled = routine?.enabled ?? schedule?.enabled;
  const readOnly = item?.project ? projects.some(entry => entry.id === item.project && entry.archived) : false;
  const disabled = busy || conflict || !!pending;
  const sourceId = routineKey || workflowKey;
  const currentWall = itemAt === undefined ? target : wallTime(itemAt, zone);
  const moveWall = target ?? currentWall;

  async function send(command: CalendarCommand, ran = false) {
    setError('');
    try {
      await request(command.path, command.body);
      setPending(undefined);
      onSaved(ran);
    } catch (reason) {
      setError(message(reason));
      if (reason instanceof HttpError && reason.status < 500) {
        setPending(undefined);
        if (reason.status === 409) setConflict(true);
      } else {
        setPending({ command, ran });
      }
    }
  }

  async function act(make: () => Promise<void> | void) {
    if (applying.current) return;
    applying.current = true;
    setBusy(true); setError('');
    try { await make(); }
    catch (reason) { setError(message(reason)); }
    finally { applying.current = false; setBusy(false); }
  }

  function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const values = new FormData(event.currentTarget);
    const field = (name: string) => String(values.get(name) ?? '');
    void act(async () => {
      if (mode === 'move' && item) {
        const destination = { date: field('date'), time: field('time') };
        const at = await resolveCalendarTime(request, destination.date, destination.time, zone);
        if (item.revision !== null && item.revision !== sourceRevision && reload === 0) {
          setConflict(true); setError('Schedule changed. Reload before moving it.'); return;
        }
        if (routine) {
          const moved = moveRoutine(routine, item.at, at, zone);
          if (!moved) {
            if (routine.schedule) setCustomMove({ spec: routine.schedule, destination });
            setTiming(null); setMode('edit');
            setError('Edit the cron expression to move this series.');
            return;
          }
          await send(routineEdit(routine, moved, zone, crypto.randomUUID()));
        } else if (workflow && schedule) {
          const nextAt = schedule.interval_ms ? Math.max(1, schedule.next_at + at - item.at) : at;
          await send(workflowEdit(workflow.id, schedule, { nextAt }, crypto.randomUUID()));
        }
        return;
      }
      if (kind === 'routine' && timing) {
        if (customMove && JSON.stringify(customMove.spec) === JSON.stringify('schedule' in timing ? timing.schedule : null)) {
          setError('Change the cron expression before saving.'); return;
        }
        const savedWall = routine && wallTime(routine.next_at, timing.zone);
        const once = 'schedule' in timing && timing.schedule.kind === 'once' && timing.schedule;
        const sameInstant = routineInitial?.kind === 'instant' && savedWall && once
          && once.date === savedWall.date && once.time === savedWall.time && timing.zone === (routine?.schedule_zone ?? zone);
        const fields = { name: field('name').trim(), prompt: field('prompt').trim(), ...(sameInstant ? {} : timing) };
        if (routine) {
          await send(routineEdit(routine, fields, zone, crypto.randomUUID()));
        } else {
          await send({ path: '/api/agents/' + owner + '/routines', body: {
            id: crypto.randomUUID(), ...fields, project: projectRecord?.id ?? null, trigger: 'schedule', enabled: true,
          } });
        }
      } else if (workflow) {
        const saved = schedule && wallTime(schedule.next_at, zone);
        const nextAt = saved && saved.date === field('date') && saved.time === field('time') ? schedule.next_at
          : await resolveCalendarTime(request, field('date'), field('time'), zone);
        await send(workflowEdit(workflow.id, schedule, {
          nextAt, intervalSeconds: field('minutes') ? Math.round(Number(field('minutes')) * 60) : null, input: field('input'),
          ...(!item && project ? { project: projectRecord?.id ?? null } : {}),
        }, crypto.randomUUID()));
      }
    });
  }
  async function toggle() {
    await act(async () => {
      if (routine) await send(routineEdit(routine, { enabled: !enabled }, zone, crypto.randomUUID()));
      if (workflow && schedule) await send(workflowEdit(workflow.id, schedule, { enabled: !enabled }, crypto.randomUUID()));
    });
  }
  async function runNow() {
    const id = crypto.randomUUID();
    await act(async () => {
      if (routine) await send({ path: '/api/routines/' + routine.id + '/test', body: { id, revision: routine.revision } }, true);
      if (workflow) await send({ path: '/api/workflows/' + workflow.id + '/run', body: { id, revision: workflow.revision, input: schedule?.input ?? '' } }, true);
    });
  }
  function close() {
    if (busy) return;
    if (!pending) { onClose(); return; }
    void confirm({ title: 'Leave unconfirmed change?', description: 'Check the calendar before repeating this action. Minarbot may have saved it.', confirm: 'Leave' }).then(ok => { if (ok) onClose(); });
  }
  function reloadSource() {
    setConflict(false); setPending(undefined); setError(''); setTiming(null); setReload(value => value + 1);
  }

  const start = schedule && (item || !target) ? wallTime(schedule.next_at, zone) : target;
  const routineInitial = routine?.schedule ?? (routine && !routine.interval_ms ? { kind: 'instant' as const, at: routine.next_at }
    : !routine && target ? { kind: 'once' as const, ...target } : undefined);
  const loaded = !!source || !sourceId || item?.editable === false;
  const canEdit = item?.editable !== false && !readOnly && !!source;
  const detail = item && mode === 'details';
  const editing = mode === 'edit';
  const moving = mode === 'move';
  const title = detail ? item.title : mode === 'run' ? 'Run inspector' : moving ? item?.recurring ? 'Move series' : 'Move occurrence'
    : item ? 'Edit schedule' : 'Add to calendar';
  const ready = moving ? !!source && !!(routine || schedule) : kind === 'routine' ? !!timing && (routine ? !!source : !!owner && !!slot?.at) : !!workflow && !!start;
  return <Dialog open title={title} size={mode === 'run' ? 'lg' : 'md'} onOpenChange={open => { if (!open) close(); }}
    description={moving && item?.recurring ? 'All future occurrences in this series will move.' : undefined}
    footer={<>
      {pending && <Button variant="primary" busy={busy} onClick={() => void act(() => send(pending.command, pending.ran))}>Retry exact action</Button>}
      {mode === 'run' && <Button onClick={() => setMode('details')}>Back to details</Button>}
      {detail && canEdit && <>
        {enabled !== undefined && <Button disabled={disabled} onClick={() => void toggle()}>{enabled ? 'Pause schedule' : 'Enable schedule'}</Button>}
        <Button disabled={disabled} onClick={() => void runNow()} busy={busy}>Run now</Button>
        <Button variant="primary" disabled={disabled} onClick={() => setMode('edit')}>Edit schedule</Button>
      </>}
      {(editing || moving) && <>
        <Button disabled={busy} onClick={() => item && !pending ? setMode('details') : close()}>{item ? 'Back' : 'Cancel'}</Button>
        <Button variant="primary" type="submit" form={form} busy={busy} disabled={disabled || !ready}>{moving ? item?.recurring ? 'Move series' : 'Move occurrence' : 'Save schedule'}</Button>
      </>}
    </>}>
    {(error || sourceError || choicesError) && <Alert tone="error" actions={(conflict || sourceError || choicesError) && <Button size="sm" onClick={reloadSource}>Reload saved schedule</Button>}>{error || sourceError || choicesError}</Alert>}
    {kind === 'routine' && slot?.error && <Alert tone="error">{slot.error}</Alert>}
    {!loaded && !sourceError && <SkeletonRows count={2} />}
    {detail && <div className="k-stack">
      <div className="k-inline"><Badge>{item.kind === 'routine' ? 'Routine' : 'Workflow'}</Badge><Badge tone={item.status === 'done' ? 'success' : item.status === 'failed' ? 'warn' : 'neutral'}>{calendarStatuses[item.status]}</Badge></div>
      <p className="calendar-detail-time"><time dateTime={new Date(item.at).toISOString()}>{new Intl.DateTimeFormat(undefined, { timeZone: zone, dateStyle: 'full', timeStyle: 'short' }).format(item.at)}</time> · {zone}</p>
      <p className="k-field__hint">{agents.find(entry => entry.id === item.agent)?.name}{item.project && ' · ' + (projects.find(entry => entry.id === item.project)?.name ?? 'Project')}</p>
      {routine && <p>{scheduleLabel(routine.schedule, routine.next_at, routine.interval_ms, routine.schedule_zone ?? zone)}</p>}
      {!!schedule?.interval_ms && <p>Every {schedule.interval_ms / 60000} minutes</p>}
      {item.count > 1 && <p>{item.count} occurrences on this date.</p>}
      <div className="k-row-actions">
        {item.run && <Button onClick={() => setMode('run')}>{item.kind === 'routine' ? 'Open Run inspector' : 'Open workflow run'}</Button>}
        {item.thread && <Button onClick={() => onThread(item.agent, item.thread!)}>Open conversation</Button>}
        {item.reportTo && item.report && <Button onClick={() => onReport(item.agent, item.reportTo!, item.report!)}>Open report</Button>}
        {!item.run && canEdit && <Button disabled={disabled} onClick={() => setMode('move')}>Move schedule</Button>}
      </div>
    </div>}
    {mode === 'run' && item?.run && (item.kind === 'routine' ? <RunInspector key={item.run} target={{ turnId: item.run }} request={request} />
      : <Suspense fallback={<SkeletonRows />}><WorkflowRunDetails id={item.run} request={request} onThread={onThread} /></Suspense>)}
    {(editing || moving) && loaded && <form id={form} className="k-stack" key={sourceId + ':' + reload + ':' + mode} onSubmit={save}>
      {editing && !item && <>
        <Field label="Automation"><Select value={kind} onChange={value => { setKind(value as CalendarKind); setWorkflowId(''); setTiming(null); }} disabled={disabled}
          options={[{ value: 'routine', label: 'New routine' }, { value: 'workflow', label: 'Schedule workflow' }]} /></Field>
        {defaultProject === '' && !!projects.length && <Field label="Project"><Select value={project} onChange={value => { setProject(value); setWorkflowId(''); }} disabled={disabled}
          options={[{ value: '', label: kind === 'routine' ? 'No project' : 'Keep workflow project' }, { value: 'none', label: 'No project', disabled: kind === 'routine' }, ...projects.filter(entry => !entry.archived).map(entry => ({ value: entry.id, label: entry.name }))].filter(option => !option.disabled)} /></Field>}
        {kind === 'routine' && <Field label="Agent"><Select value={owner} onChange={setAgent} disabled={disabled || !availableAgents.length}
          options={availableAgents.map(entry => ({ value: entry.id, label: entry.name }))} placeholder="No available agents" /></Field>}
        {kind === 'workflow' && <Field label="Workflow"><Select value={workflowId} onChange={setWorkflowId} disabled={disabled || !workflows?.length}
          options={[{ value: '', label: 'Choose workflow' }, ...(workflows ?? []).map(entry => ({ value: entry.id, label: entry.name }))]} /></Field>}
        {kind === 'workflow' && workflows?.length === 0 && <p className="k-field__hint">No workflows available for this project.</p>}
      </>}
      {moving && moveWall && <>
        <p className="k-field__hint">{item?.title} · {zone}</p>
        <div className="k-grid-2">
          <Field label="Date"><Input type="date" name="date" required defaultValue={moveWall.date} disabled={disabled} /></Field>
          <Field label="Time"><Input type="time" name="time" required defaultValue={moveWall.time} disabled={disabled} /></Field>
        </div>
      </>}
      {editing && kind === 'routine' && <>
        {customMove && <p className="k-field__hint">Move series to {dateLabel(customMove.destination.date)} at {customMove.destination.time} ({zone}).</p>}
        <Field label="Name"><Input name="name" required maxLength={80} defaultValue={routine?.name} disabled={disabled} /></Field>
        <Field label="Prompt"><Textarea name="prompt" required maxLength={16000} defaultValue={routine?.prompt} disabled={disabled} /></Field>
        <RoutineSchedule initial={routineInitial} initialNextAt={routine?.next_at ?? itemAt ?? 0}
          initialInterval={routine?.interval_ms ?? null} initialZone={routine?.schedule_zone ?? zone}
          intervalAnchor={slot?.at}
          legacy={!!routine && !routine.schedule} active disabled={disabled} onChange={setTiming} />
      </>}
      {editing && kind === 'workflow' && workflow && start && <>
        <div className="k-grid-2">
          <Field label={'Date (' + zone + ')'}><Input type="date" name="date" required defaultValue={start.date} disabled={disabled} /></Field>
          <Field label="Time"><Input type="time" name="time" required defaultValue={start.time} disabled={disabled} /></Field>
        </div>
        <Field label="Repeat every (minutes)" hint="Leave empty to run once."><Input type="number" name="minutes" min={1} max={525600} step="any" defaultValue={schedule?.interval_ms ? schedule.interval_ms / 60000 : ''} disabled={disabled} /></Field>
        <Field label="Scheduled input" optional><Textarea name="input" maxLength={16000} defaultValue={schedule?.input} disabled={disabled} /></Field>
        {!!schedule && !item && <p className="k-field__hint">This changes the workflow’s existing schedule.</p>}
        {!!schedule && !schedule.enabled && <p className="k-field__hint">This schedule is paused. Enable it from its calendar item.</p>}
      </>}
    </form>}
  </Dialog>;
}

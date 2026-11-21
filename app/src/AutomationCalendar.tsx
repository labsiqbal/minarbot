import { useId, useMemo, useRef, useState, type DragEvent } from 'react';
import { CalendarDays, ChevronLeft, ChevronRight, Download, Plus, Workflow } from 'lucide-react';
import type { LocalProject } from '../shared/local-project';
import { AutomationCalendarDialog, type CalendarSelection } from './AutomationCalendarDialog';
import { addDays, calendarClock, calendarStatuses, dateLabel, stepCalendar,
  type CalendarAgent, type CalendarItem, type CalendarRequest, type CalendarView } from './automation-calendar';
import { useAutomationCalendar } from './hooks/useAutomationCalendar';
import { Alert, Button, EmptyState, IconButton, PageHeader, Select, SkeletonRows } from './ui/kit';
import './AutomationCalendar.css';

const views = [{ value: 'month', label: 'Month' }, { value: 'week', label: 'Week' }, { value: 'day', label: 'Day' }] as const;
const matchAll = (label: string) => ({ value: '', label });
const hourTime = (hour: number) => String(hour).padStart(2, '0') + ':00';

export function AutomationCalendar({ project, projects, agents, request, onThread, onReport }: {
  project?: LocalProject; projects: LocalProject[]; agents: CalendarAgent[];
  request: CalendarRequest; onThread: (agent: string, thread: string) => void; onReport: (agent: string, destination: string, turn: string) => void;
}) {
  const [view, setView] = useState<CalendarView>('month');
  const [date, setDate] = useState<string>();
  const [filters, setFilters] = useState({ project: project?.id ?? '', agent: '', kind: '', status: '' });
  const [selection, setSelection] = useState<CalendarSelection>();
  const [notice, setNotice] = useState('');
  const [dragging, setDragging] = useState<CalendarItem>();
  const [dropTarget, setDropTarget] = useState<string>();
  const heading = useRef<HTMLHeadingElement>(null);
  const titleId = useId();
  const { data, zone, error, refresh, today, focusDate, range } = useAutomationCalendar(request, date, view, filters);
  const members = project ? agents.filter(agent => project.members.some(member => member.agent === agent.id)) : agents;
  const clock = useMemo(() => calendarClock(zone ?? 'UTC'), [zone]);
  const byDay = useMemo(() => {
    const days = new Map<string, CalendarItem[]>();
    for (const item of data?.items ?? []) {
      const { date } = clock(item.at);
      const current = days.get(date);
      if (current) current.push(item); else days.set(date, [item]);
    }
    return days;
  }, [clock, data]);
  const statusLabel = Object.entries(calendarStatuses).map(([value, label]) => ({ value, label }));
  const groupLabel = view === 'month' ? dateLabel(focusDate, { month: 'long', year: 'numeric' })
    : view === 'day' ? dateLabel(focusDate, { weekday: 'long', month: 'long', day: 'numeric' })
    : dateLabel(range.start, { month: 'short', day: 'numeric' }) + ' – ' + dateLabel(addDays(range.end, -1), { month: 'short', day: 'numeric', year: 'numeric' });
  const create = (date: string, time = '09:00') => {
    if (zone && !project?.archived) setSelection({ slot: { date, time } });
  };
  function filter(key: keyof typeof filters, value: string) {
    setFilters(current => ({ ...current, [key]: value }));
  }
  function allowDrop(event: DragEvent, day: string, time?: string) {
    if (!dragging) return;
    event.preventDefault();
    event.dataTransfer.dropEffect = 'move';
    setDropTarget(day + (time ?? ''));
  }
  function drop(event: DragEvent, day: string, time?: string) {
    event.preventDefault();
    event.stopPropagation();
    if (!dragging || !zone) return;
    const origin = clock(dragging.at);
    const target = { date: day, time: time ?? origin.time };
    if (origin.date !== target.date || origin.time !== target.time) setSelection({ item: dragging, target });
    setDragging(undefined); setDropTarget(undefined);
  }
  function dropProps(day: string, time?: string) {
    const key = day + (time ?? '');
    return {
      'data-drop': dropTarget === key || undefined,
      onDragOver: (event: DragEvent) => { event.stopPropagation(); allowDrop(event, day, time); },
      onDragLeave: (event: DragEvent) => { if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setDropTarget(undefined); },
      onDrop: (event: DragEvent) => drop(event, day, time),
    };
  }
  function eventButton(item: CalendarItem, compact = false) {
    const time = clock(item.at).time;
    const label = calendarStatuses[item.status];
    const kind = item.kind === 'routine' ? 'Routine' : 'Workflow';
    const person = agents.find(agent => agent.id === item.agent)?.name;
    const context = compact ? label : [label, person, !project && projects.find(entry => entry.id === item.project)?.name].filter(Boolean).join(' · ');
    const draggable = item.status === 'scheduled' && item.editable && !project?.archived;
    return <button className="calendar-event" type="button" key={item.id} data-status={item.status}
      aria-label={[item.title, kind, label, time, item.count > 1 && item.count + ' occurrences'].filter(Boolean).join(', ')}
      title={[item.title, kind, context].join(' · ')} draggable={draggable}
      onDragStart={event => {
        if (!draggable) return;
        event.dataTransfer.setData('text/plain', item.title);
        event.dataTransfer.effectAllowed = 'move'; setDragging(item);
      }}
      onDragEnd={() => { setDragging(undefined); setDropTarget(undefined); }}
      onClick={() => setSelection({ item })}>
      <span className="calendar-event__main">
        {item.kind === 'routine' ? <CalendarDays size={13} aria-hidden /> : <Workflow size={13} aria-hidden />}
        <span className="calendar-event__name">{item.title}</span>
      </span>
      <span className="calendar-event__meta"><span>{item.count > 1 ? item.count + ' runs' : time}</span><span className="calendar-event__status">{context}</span></span>
    </button>;
  }
  function addSlot(day: string, time?: string) {
    return <IconButton className="calendar-add" label={'Add automation on ' + dateLabel(day) + (time ? ' at ' + time : '')} tooltip="Add automation" disabled={!zone || !!project?.archived}
      onClick={() => create(day, time)}><Plus size={14} aria-hidden /></IconButton>;
  }
  const liveItem = selection && 'item' in selection && !selection.target
    ? data?.items.find(item => item.id === selection.item.id)
    : undefined;
  const agendaDays = range.days.filter(day => byDay.has(day));
  if (!agendaDays.length) agendaDays.push(focusDate);
  const byHour = new Map<string, CalendarItem[]>();
  if (view === 'day') for (const item of byDay.get(focusDate) ?? []) {
    const hour = clock(item.at).time.slice(0, 2) + ':00';
    const current = byHour.get(hour);
    if (current) current.push(item); else byHour.set(hour, [item]);
  }
  const main = <section aria-labelledby={titleId} className="automation-calendar">
    <div className="calendar-toolbar">
      <div className="calendar-period">
        <div className="calendar-period__nav">
          <IconButton label={'Previous ' + view} onClick={() => setDate(stepCalendar(focusDate, view, -1))}><ChevronLeft size={17} aria-hidden /></IconButton>
          <IconButton label={'Next ' + view} onClick={() => setDate(stepCalendar(focusDate, view, 1))}><ChevronRight size={17} aria-hidden /></IconButton>
          <Button size="sm" variant="ghost" onClick={() => setDate(today)}>Today</Button>
        </div>
        <h2 id={titleId} aria-live="polite">{groupLabel}</h2>
      </div>
      <div className="calendar-view" role="group" aria-label="Calendar view">
        {views.map(entry => <Button key={entry.value} size="sm" variant="ghost" aria-pressed={entry.value === view} onClick={() => setView(entry.value)}>{entry.label}</Button>)}
      </div>
    </div>
    <div className="calendar-filters">
      {!project && <Select size="sm" label="Filter by project" value={filters.project} onChange={value => filter('project', value)}
        options={[matchAll('All projects'), { value: 'none', label: 'No project' }, ...projects.map(project => ({ value: project.id, label: project.name }))]} />}
      <Select size="sm" label="Filter by agent" value={filters.agent} onChange={value => filter('agent', value)}
        options={[matchAll('All agents'), ...members.map(agent => ({ value: agent.id, label: agent.name }))]} />
      <Select size="sm" label="Filter by kind" value={filters.kind} onChange={value => filter('kind', value)}
        options={[matchAll('All kinds'), { value: 'routine', label: 'Routines' }, { value: 'workflow', label: 'Workflows' }]} />
      <Select size="sm" label="Filter by status" value={filters.status} onChange={value => filter('status', value)}
        options={[matchAll('All statuses'), ...statusLabel]} />
      {zone && <span className="calendar-zone" title="Home device time zone">{zone}</span>}
    </div>
    {error && <Alert tone="error" actions={<Button size="sm" onClick={refresh}>Retry</Button>}>{error}</Alert>}
    {data?.truncated && <Alert tone="warn">Showing up to 500 items. Use filters or a shorter view to see more.</Alert>}
    <div className="sr-only" role="status">{notice}</div>
    {!data && !error && <SkeletonRows count={3} />}
    {data && <>
      {view === 'day' ? <ol className="calendar-day calendar-wide" aria-label="Hourly calendar">
        {Array.from({ length: 24 }, (_, hour) => {
          const time = hourTime(hour);
          const events = byHour.get(time) ?? [];
          return <li key={time} {...dropProps(focusDate, time)} onClick={event => {
            if (!(event.target as Element).closest('button')) create(focusDate, time);
          }}>
            <span className="calendar-day__hour">{time}</span>
            <div className="calendar-day__events">{events.map(item => eventButton(item))}</div>
            {addSlot(focusDate, time)}
          </li>;
        })}
      </ol> : <div className={'calendar-grid calendar-wide calendar-grid--' + view} aria-label={view === 'month' ? 'Month calendar' : 'Week calendar'}>
        {range.days.slice(0, 7).map(day => <div className="calendar-grid__weekday" key={'day:' + day}>{dateLabel(day, { weekday: 'short' })}</div>)}
        {range.days.map(day => {
          const items = byDay.get(day) ?? [];
          const outside = view === 'month' && day.slice(0, 7) !== focusDate.slice(0, 7);
          return <section className="calendar-cell" key={day} data-outside={outside || undefined} aria-label={dateLabel(day)} {...dropProps(day)}
            onClick={event => { if (event.target === event.currentTarget) create(day); }}>
            <div className="calendar-cell__head">
              <Button size="sm" variant="ghost" aria-label={'View ' + dateLabel(day)} aria-current={day === today ? 'date' : undefined} onClick={() => { setDate(day); setView('day'); }}>
                {Number(day.slice(-2))}
              </Button>
              {addSlot(day)}
            </div>
            {items.slice(0, view === 'month' ? 3 : items.length).map(item => eventButton(item, true))}
            {view === 'month' && items.length > 3 && <Button className="calendar-more" size="sm" variant="ghost" onClick={() => { setDate(day); setView('day'); }}>+{items.length - 3} more</Button>}
          </section>;
        })}
      </div>}
      <div className="calendar-agenda" aria-label="Agenda">
        {agendaDays.map(day => <section key={day} className="calendar-agenda__date" aria-label={dateLabel(day)}>
          <div className="calendar-agenda__head"><h3>{dateLabel(day, { weekday: 'long', month: 'short', day: 'numeric' })}</h3>{addSlot(day)}</div>
          {byDay.get(day)?.map(item => eventButton(item))}
          {!byDay.has(day) && <EmptyState title="No automations in this range" />}
        </section>)}
      </div>
    </>}
    {selection && zone && <AutomationCalendarDialog selection={liveItem ? { item: liveItem } : selection} zone={zone} agents={agents} projects={projects}
      defaultProject={project?.id ?? filters.project} defaultAgent={filters.agent} request={request} onClose={() => setSelection(undefined)}
      onSaved={ran => {
        setNotice(ran ? 'Run requested.' : 'Schedule saved.');
        setSelection(undefined);
        if (ran) { setDate(today); filter('status', ''); }
        refresh();
      }} onThread={onThread} onReport={onReport} />}
  </section>;
  if (project) return <div className="calendar-in-project">
    <div className="calendar-actions">
      <Button size="sm" variant="primary" disabled={!zone || project.archived} icon={<Plus size={15} aria-hidden />} onClick={() => create(focusDate)}>Add automation</Button>
      <a className="k-btn k-btn--secondary k-btn--sm" href={'/api/automation-calendar/routines.ics?' + new URLSearchParams({ project: project.id })} download="minarbot-routines.ics"><Download size={14} aria-hidden />Download routines</a>
    </div>
    {main}
  </div>;
  return <div className="k-page bs-root calendar-page">
    <PageHeader headingRef={heading} title="Automation calendar" actions={<Button size="sm" variant="primary" disabled={!zone} icon={<Plus size={15} aria-hidden />} onClick={() => create(focusDate)}>Add automation</Button>} />
    {main}
  </div>;
}

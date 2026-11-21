import type { Spec } from '../shared/routine-schedule.ts';

export type CalendarRequest = <T>(path: string, data?: unknown, signal?: AbortSignal) => Promise<T>;
export type CalendarView = 'month' | 'week' | 'day';
export type CalendarKind = 'routine' | 'workflow';
export const calendarStatuses = { scheduled: 'Scheduled', running: 'Running', done: 'Done', failed: 'Failed', paused: 'Paused', no_update: 'No update' } as const;
export type CalendarItem = {
  id: string; sourceId: string; kind: CalendarKind; title: string; agent: string; project: string | null;
  at: number; status: keyof typeof calendarStatuses; count: number; run: string | null; thread: string | null;
  reportTo: string | null; report: string | null; revision: number | null; recurring: boolean; editable: boolean;
};
export type CalendarResponse = { zone: string; now: number; items: CalendarItem[]; truncated: boolean };
export type CalendarAgent = { id: string; name: string; hidden?: boolean };
export type CalendarRoutine = {
  id: string; name: string; prompt: string; agent: string; project: string | null; report_to: string;
  trigger: string; revision: number; enabled: number; schedule: Spec | null; schedule_zone: string | null;
  next_at: number; interval_ms: number | null;
};
export type CalendarWorkflow = { id: string; name: string; agent: string; revision: number; schedule_project?: string | null };
export type CalendarWorkflowSchedule = { revision: number; next_at: number; interval_ms: number | null; input: string; enabled: number; project: string | null };
export type CalendarCommand = { path: string; body: Record<string, unknown> };

export const dayMs = 86_400_000;
const civil = (day: string) => new Date(day + 'T00:00:00Z');
export const dateKey = (date: Date) => date.toISOString().slice(0, 10);
export const addDays = (day: string, count: number) => dateKey(new Date(civil(day).getTime() + count * dayMs));

export function calendarClock(zone: string) {
  const options: Intl.DateTimeFormatOptions = { timeZone: zone, year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hourCycle: 'h23' };
  const formatter = new Intl.DateTimeFormat('en-CA', options);
  return (at: number) => {
    const { year, month, day, hour, minute } = Object.fromEntries(formatter.formatToParts(at).map(part => [part.type, part.value]));
    return { date: [year.padStart(4, '0'), month, day].join('-'), time: [hour, minute].join(':') };
  };
}

export const wallTime = (at: number, zone: string) => calendarClock(zone)(at);

export function dateLabel(day: string, options: Intl.DateTimeFormatOptions = { dateStyle: 'full' }) {
  return new Intl.DateTimeFormat(undefined, { ...options, timeZone: 'UTC' }).format(civil(day));
}

function monday(day: string) { return addDays(day, -((civil(day).getUTCDay() + 6) % 7)); }

export function calendarRange(date: string, view: CalendarView) {
  if (view === 'day') return { start: date, end: addDays(date, 1), days: [date] };
  const start = monday(view === 'month' ? date.slice(0, 7) + '-01' : date);
  const days = Array.from({ length: view === 'month' ? 42 : 7 }, (_, i) => addDays(start, i));
  return { start, end: addDays(start, days.length), days };
}

export function stepCalendar(date: string, view: CalendarView, step: number) {
  if (view !== 'month') return addDays(date, step * (view === 'week' ? 7 : 1));
  const first = civil(date.slice(0, 7) + '-01');
  first.setUTCMonth(first.getUTCMonth() + step);
  return dateKey(first);
}

export async function resolveCalendarTime(request: CalendarRequest, date: string, time: string, zone: string, signal?: AbortSignal) {
  const query = new URLSearchParams({ zone, schedule: JSON.stringify({ kind: 'once', date, time }) });
  const { at } = await request<{ at: number }>('/api/routine-schedule?' + query, undefined, signal);
  return at;
}

export function routineTiming(routine: CalendarRoutine, zone: string) {
  const preferred = routine.schedule_zone ?? zone;
  return routine.schedule && routine.schedule.kind !== 'legacy_interval'
    ? { zone: preferred, schedule: routine.schedule }
    : { zone: preferred, nextAt: routine.next_at, intervalSeconds: routine.interval_ms === null ? null : routine.interval_ms / 1000 };
}

export function routineEdit(routine: CalendarRoutine, fields: Record<string, unknown>, zone: string, id: string): CalendarCommand {
  return {
    path: '/api/routines/' + routine.id + '/edit',
    body: { id, revision: routine.revision, name: routine.name, prompt: routine.prompt, enabled: !!routine.enabled,
      ...('schedule' in fields || 'nextAt' in fields ? {} : routineTiming(routine, zone)), ...fields },
  };
}

export function workflowEdit(workflow: string, schedule: CalendarWorkflowSchedule | null, fields: Record<string, unknown>, id: string): CalendarCommand {
  return {
    path: '/api/workflows/' + workflow + '/schedule',
    body: { id, revision: schedule?.revision ?? 0, nextAt: schedule?.next_at, intervalSeconds: schedule?.interval_ms ? schedule.interval_ms / 1000 : null, input: schedule?.input ?? '', enabled: schedule ? !!schedule.enabled : true, ...fields },
  };
}

export function moveCron(expression: string, from: string, date: string, time: string) {
  const match = /^(\d+) (\d+) (\*|\d+) (\*|\d+) (\*|1-5|[0-6](?:,[0-6])*)$/.exec(expression);
  if (!match) return null;
  const days = (civil(date).getTime() - civil(from).getTime()) / dayMs;
  const [, , , monthDay, month, weekdays] = match;
  if (days && (monthDay !== '*' || month !== '*')) return null;
  const offset = (days % 7 + 7) % 7;
  const newDays = weekdays === '*' || !offset ? weekdays : (weekdays === '1-5' ? [1, 2, 3, 4, 5] : weekdays.split(',').map(Number))
    .map(day => (day + offset) % 7).sort().join(',');
  const [hour, minute] = time.split(':').map(Number);
  return [minute, hour, monthDay, month, newDays].join(' ');
}

export function moveRoutine(routine: CalendarRoutine, itemAt: number, at: number, zone: string): Record<string, unknown> | null {
  const timing = routineTiming(routine, zone);
  const spec = routine.schedule;
  if (spec?.kind === 'cron') {
    const { date, time } = wallTime(at, timing.zone);
    const expression = moveCron(spec.expression.trim().replace(/\s+/g, ' '), wallTime(itemAt, timing.zone).date, date, time);
    return expression === null ? null : { zone: timing.zone, schedule: { kind: 'cron', expression } };
  }
  if (spec?.kind === 'once') return { zone: timing.zone, schedule: { kind: 'once', ...wallTime(at, timing.zone) } };
  if (spec?.kind === 'instant') return { zone: timing.zone, schedule: { kind: 'instant', at } };
  const next = Math.max(1, routine.next_at + at - itemAt);
  if (spec?.kind === 'interval') return { zone: timing.zone, schedule: { ...spec, anchorAt: next } };
  return { zone: timing.zone, nextAt: routine.interval_ms ? next : at, intervalSeconds: routine.interval_ms === null ? null : routine.interval_ms / 1000 };
}

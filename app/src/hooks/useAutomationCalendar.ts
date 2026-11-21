import { useCallback, useEffect, useRef, useState } from 'react';
import { usePolling } from './usePolling';
import { calendarRange, resolveCalendarTime, wallTime, type CalendarRequest, type CalendarResponse, type CalendarRoutine, type CalendarView, type CalendarWorkflow, type CalendarWorkflowSchedule } from '../automation-calendar';

const message = (error: unknown, fallback: string) => error instanceof Error ? error.message : fallback;

export function useAutomationCalendar(request: CalendarRequest, date: string | undefined, view: CalendarView, filters: Record<string, string>) {
  const [result, setResult] = useState<{ path: string; value: CalendarResponse }>();
  const [error, setError] = useState('');
  const [version, setVersion] = useState(0);
  const refresh = useCallback(() => setVersion(value => value + 1), []);
  const [live, setLive] = useState(false);
  const loading = useRef(false);
  const invalidated = useRef(false);
  const zone = result?.value.zone;
  const today = wallTime(Date.now(), zone ?? Intl.DateTimeFormat().resolvedOptions().timeZone).date;
  const focusDate = date ?? today;
  const range = calendarRange(focusDate, view);
  const query = new URLSearchParams({ start: range.start, end: range.end });
  for (const [key, value] of Object.entries(filters)) if (value) query.set(key, value);
  const path = '/api/automation-calendar?' + query;

  useEffect(() => {
    const source = new EventSource('/api/automation-calendar/stream');
    source.onmessage = () => {
      setLive(true);
      if (loading.current) invalidated.current = true;
      else refresh();
    };
    source.onerror = () => setLive(false);
    return () => source.close();
  }, [refresh]);

  usePolling(async signal => {
    loading.current = true;
    try {
      const value = await request<CalendarResponse>(path, undefined, signal);
      if (!signal.aborted) { setResult({ path, value }); setError(''); }
    } catch (error) {
      if (!signal.aborted) setError(message(error, 'Calendar could not load.'));
    } finally {
      loading.current = false;
      if (invalidated.current) {
        invalidated.current = false;
        if (!signal.aborted) refresh();
      }
    }
  }, { interval: live ? 45_000 : 5_000 }, [request, path, version]);

  return { data: result?.path === path ? result.value : undefined, zone, error, refresh, today, focusDate, range };
}

type Source = { routine: CalendarRoutine } | { workflow: CalendarWorkflow; schedule: CalendarWorkflowSchedule | null };

export function useCalendarSlot(request: CalendarRequest, date: string | undefined, time: string | undefined, zone: string) {
  const [result, setResult] = useState<{ at?: number; error?: string }>();
  useEffect(() => {
    if (!date || !time) return;
    const controller = new AbortController();
    void resolveCalendarTime(request, date, time, zone, controller.signal)
      .then(at => { if (!controller.signal.aborted) setResult({ at }); })
      .catch(reason => { if (!controller.signal.aborted) setResult({ error: message(reason, 'Time could not load.') }); });
    return () => controller.abort();
  }, [request, date, time, zone]);
  return result;
}

export function useCalendarSource(request: CalendarRequest, routine: string, workflow: string, enabled: boolean, reload: number) {
  const key = (routine || workflow) + ':' + reload;
  const [result, setResult] = useState<{ key: string; source?: Source; error: string }>();
  useEffect(() => {
    if ((!routine && !workflow) || !enabled) return;
    const controller = new AbortController();
    const { signal } = controller;
    const get = <T,>(path: string) => request<T>(path, undefined, signal);
    const load: Promise<Source> = routine
      ? get<CalendarRoutine>('/api/routines/' + routine).then(value => ({ routine: value }))
      : Promise.all([get<CalendarWorkflow>('/api/workflows/' + workflow), get<{ schedule: CalendarWorkflowSchedule | null }>('/api/workflows/' + workflow + '/schedule')])
        .then(([value, { schedule }]) => ({ workflow: value, schedule }));
    void load.then(source => { if (!signal.aborted) setResult({ key, source, error: '' }); })
      .catch(reason => { if (!signal.aborted) setResult({ key, error: message(reason, 'Schedule could not load.') }); });
    return () => controller.abort();
  }, [key, request, routine, workflow, enabled]);
  return result?.key === key ? result : { source: undefined, error: '' };
}

export function useCalendarWorkflows(request: CalendarRequest, enabled: boolean, project: string, reload: number) {
  const key = project + ':' + reload;
  const [result, setResult] = useState<{ key: string; workflows?: CalendarWorkflow[]; error: string }>();
  useEffect(() => {
    if (!enabled) return;
    const controller = new AbortController();
    const { signal } = controller;
    void (async () => {
      const rows: CalendarWorkflow[] = [];
      let after = '';
      do {
        const query = new URLSearchParams();
        if (project) query.set('project', project);
        if (after) query.set('after', after);
        const page = await request<{ workflows: CalendarWorkflow[]; next_after: string | null }>('/api/workflows?' + query, undefined, signal);
        if (signal.aborted) return;
        rows.push(...page.workflows);
        after = page.next_after ?? '';
      } while (after);
      setResult({ key, workflows: rows, error: '' });
    })().catch(reason => { if (!signal.aborted) setResult({ key, error: message(reason, 'Workflows could not load.') }); });
    return () => controller.abort();
  }, [request, enabled, project, key]);
  return result?.key === key ? result : { workflows: undefined, error: '' };
}

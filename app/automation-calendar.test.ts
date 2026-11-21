import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { startApp } from './test-bridge.ts';
import { HttpError, readJson } from './src/http.ts';
import { addDays, moveRoutine, resolveCalendarTime, routineEdit, wallTime, workflowEdit,
  type CalendarRequest, type CalendarResponse, type CalendarRoutine, type CalendarWorkflowSchedule } from './src/automation-calendar.ts';

test('calendar creates, edits, moves and pauses both kinds using the saved revision', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'minarbot-calendar-test-'));
  const now = Date.parse('2026-11-10T03:00:00Z');
  const app = await startApp({ database: join(directory, 'state.sqlite'), fixedClock: now }, { directory });
  const request: CalendarRequest = async (path, body) => readJson(await fetch(app.origin + path, body === undefined ? {} : {
    method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(body),
  }));
  const command = <T,>({ path, body }: { path: string; body: unknown }) => request<T>(path, body);
  const conflict = (error: unknown) => error instanceof HttpError && error.status === 409;
  try {
    const agent = await request<{ id: string }>('/api/agents', { id: randomUUID(), runtime: 'api', name: 'Owner', model: 'test', instructions: 'Help.' });
    const project = await request<{ id: string }>('/api/projects', { id: randomUUID(), name: 'Schedule test', members: [{ agent: agent.id, write: false }] });
    const otherProject = await request<{ id: string }>('/api/projects', { id: randomUUID(), name: 'Other project', members: [] });
    const { zone } = await request<{ zone: string }>('/api/routine-schedule');
    const target = addDays(wallTime(now, zone).date, 1);
    const moved = addDays(target, 1);
    const start = await resolveCalendarTime(request, target, '09:15', zone);
    const end = await resolveCalendarTime(request, moved, '10:15', zone);
    const range = (fields: Record<string, string> = {}) => request<CalendarResponse>('/api/automation-calendar?' +
      new URLSearchParams({ start: target, end: addDays(target, 7), ...fields }));
    const assertItem = async (id: string, kind: string, at: number, status: string, title?: string) => {
      const { items } = await range({ project: project.id, agent: agent.id, kind, status });
      const item = items.find(row => row.sourceId === id && row.at === at);
      assert(item, JSON.stringify(items));
      if (title) assert.equal(item.title, title);
    };

    let routine = await request<CalendarRoutine>('/api/agents/' + agent.id + '/routines', {
      id: randomUUID(), name: 'Morning routine', prompt: 'Summarize.', project: project.id,
      schedule: { kind: 'once', date: target, time: '09:15' }, zone, enabled: true,
    });
    await assertItem(routine.id, 'routine', start, 'scheduled');
    const staleRoutine = routineEdit(routine, { enabled: false }, zone, randomUUID());
    routine = await command(routineEdit(routine, { name: 'Edited routine' }, zone, randomUUID()));
    await assert.rejects(command(staleRoutine), conflict);
    await assertItem(routine.id, 'routine', start, 'scheduled', 'Edited routine');
    const movedTiming = moveRoutine(routine, start, end, zone);
    assert(movedTiming);
    routine = await command(routineEdit(routine, movedTiming, zone, randomUUID()));
    await assertItem(routine.id, 'routine', end, 'scheduled');
    await command(routineEdit(routine, { enabled: false }, zone, randomUUID()));
    await assertItem(routine.id, 'routine', end, 'paused');

    const workflow = await request<{ id: string }>('/api/agents/' + agent.id + '/workflows', {
      id: randomUUID(), revision: 0, name: 'Calendar workflow', graph: { entry: 'output', maxSteps: 8, nodes: [{ id: 'output', kind: 'output' }] },
    });
    await assert.rejects(command(workflowEdit(workflow.id, null, { nextAt: start, project: otherProject.id }, randomUUID())),
      (error: unknown) => error instanceof HttpError && error.status === 403);
    let schedule = await command<CalendarWorkflowSchedule>(workflowEdit(workflow.id, null, { nextAt: start, project: project.id, intervalSeconds: 3600 }, randomUUID()));
    await assertItem(workflow.id, 'workflow', start, 'scheduled');
    assert.equal((await range({ project: otherProject.id })).items.length, 0);
    const staleWorkflow = workflowEdit(workflow.id, schedule, { enabled: false }, randomUUID());
    schedule = await command(workflowEdit(workflow.id, schedule, { input: 'New workflow input' }, randomUUID()));
    assert.equal(schedule.project, project.id);
    assert.equal(schedule.input, 'New workflow input');
    await assert.rejects(command(staleWorkflow), conflict);
    schedule = await command(workflowEdit(workflow.id, schedule, { nextAt: end }, randomUUID()));
    await assertItem(workflow.id, 'workflow', end, 'scheduled');
    await command(workflowEdit(workflow.id, schedule, { enabled: false }, randomUUID()));
    await assertItem(workflow.id, 'workflow', end, 'paused');
    assert.equal((await range({ status: 'paused' })).items.length, 2);
  } finally {
    await app.close();
    rmSync(directory, { recursive: true, force: true });
  }
});

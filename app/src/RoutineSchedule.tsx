import { useEffect, useState } from 'react';
import type { Spec } from '../shared/routine-schedule';
import { readJson } from './http';
import { Alert, Checkbox, Field, Input, Select } from './ui/kit';

export type RoutineTiming = { schedule: Spec; zone: string } | { nextAt: number; intervalSeconds: number | null; zone: string };
const days = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
export function scheduleLabel(schedule: Spec | null | undefined, nextAt: number, duration: number | null, zone: string | null | undefined) {
  const timeZone = zone || Intl.DateTimeFormat().resolvedOptions().timeZone;
  if (!schedule) return duration ? `Every ${duration / 60000} minutes` : `Once on ${new Date(nextAt).toLocaleString('en-GB', { timeZone })} (${timeZone})`;
  if (schedule.kind === 'once') return `Once on ${schedule.date} at ${schedule.time} (${timeZone})`;
  if (schedule.kind === 'cron') {
    const match = /^(\d+) (\d+) \* \* (\*|1-5|[0-6](?:,[0-6])*)$/.exec(schedule.expression);
    if (!match) return `Cron: ${schedule.expression} (${timeZone})`;
    const clock = `${match[2].padStart(2,'0')}:${match[1].padStart(2,'0')}`;
    return `${match[3] === '*' ? 'Every day' : match[3] === '1-5' ? 'Weekdays' : `Weekly on ${match[3].split(',').map(day => days[Number(day)]).join(', ')}`} at ${clock} (${timeZone})`;
  }
  if (schedule.kind === 'interval') return `Every ${schedule.amount} ${schedule.unit}${schedule.amount === 1 ? '' : 's'}`;
  if (schedule.kind === 'legacy_interval') return `Every ${schedule.intervalMs / 60000} minutes`;
  return `Once on ${new Date(schedule.at).toLocaleString('en-GB', { timeZone })} (${timeZone})`;
}
export function RoutineSchedule({ initial, initialNextAt, initialInterval, initialZone, intervalAnchor, legacy, disabled, active, onChange }: {
  initial?: Spec | null; initialNextAt: number; initialInterval: number | null; initialZone?: string | null;
  intervalAnchor?: number;
  legacy: boolean; disabled: boolean; active: boolean; onChange: (value: RoutineTiming | null) => void;
}) {
  const cron = initial?.kind === 'cron' ? /^(\d+) (\d+) \* \* (\*|1-5|[0-6](?:,[0-6])*)$/.exec(initial.expression) : null;
  const [mode,setMode] = useState(initial?.kind === 'cron' ? cron ? cron[3] === '*' ? 'daily' : cron[3] === '1-5' ? 'weekdays' : 'weekly' : 'cron' : legacy && initialInterval ? 'legacy' : initial?.kind === 'interval' ? 'interval' : initial ? 'once' : 'daily');
  const [zone,setZone] = useState(initialZone ?? ''), [zoneError,setZoneError] = useState('');
  const [date,setDate] = useState(initial?.kind === 'once' ? initial.date : ''), [clock,setClock] = useState(initial?.kind === 'once' ? initial.time : cron ? `${cron[2].padStart(2,'0')}:${cron[1].padStart(2,'0')}` : '09:00');
  const [weekly,setWeekly] = useState(cron && !['*','1-5'].includes(cron[3]) ? cron[3].split(',') : ['1']);
  const [expression,setExpression] = useState(initial?.kind === 'cron' ? initial.expression : '0 9 * * 1-5');
  const [amount,setAmount] = useState(initial?.kind === 'interval' ? String(initial.amount) : initialInterval ? String(initialInterval / 60000) : '1');
  const [unit,setUnit] = useState(initial?.kind === 'interval' ? initial.unit : 'hour');
  const [anchor] = useState(initial?.kind === 'interval' ? initial.anchorAt : Date.now());
  const [preview,setPreview] = useState<{ times: number[]; zone: string } | null>(null), [error,setError] = useState('');
  useEffect(() => {
    if (initialZone) return;
    let stopped = false;
    void fetch('/api/routine-schedule').then(response => readJson<{ zone: string }>(response)).then(result => { if (!stopped) setZone(result.zone); }).catch(error => { if (!stopped) setZoneError(error.message); });
    return () => { stopped = true; };
  }, [initialZone]);
  useEffect(() => {
    if (!zone || date) return;
    try { new Intl.DateTimeFormat('en', { timeZone: zone }); } catch { return; }
    const parts = Object.fromEntries(new Intl.DateTimeFormat('en-CA', { timeZone: zone, year:'numeric',month:'2-digit',day:'2-digit' }).formatToParts(initialNextAt).map(part => [part.type,part.value]));
    setDate(`${parts.year}-${parts.month}-${parts.day}`);
    if (initial?.kind === 'instant' || legacy && !initialInterval) setClock(new Intl.DateTimeFormat('en-GB',{timeZone:zone,hour:'2-digit',minute:'2-digit',hourCycle:'h23'}).format(initialNextAt));
  }, [zone, date, initialNextAt, initial, legacy, initialInterval]);
  let spec: Spec;
  if (mode === 'legacy') spec = { kind:'legacy_interval',intervalMs:Number(amount)*60000,anchorAt:initialNextAt };
  else if (mode === 'once') spec = { kind:'once',date,time:clock };
  else if (mode === 'interval') spec = { kind:'interval',amount:Number(amount),unit:unit as 'minute'|'hour'|'day',anchorAt:initial?.kind === 'interval' && Number(amount) === initial.amount && unit === initial.unit ? initial.anchorAt : intervalAnchor ?? anchor + Number(amount) * (unit === 'minute' ? 60000 : unit === 'hour' ? 3600000 : 86400000) };
  else spec = { kind:'cron',expression:mode === 'cron' ? expression : `${Number(clock.split(':')[1])} ${Number(clock.split(':')[0])} * * ${mode === 'daily' ? '*' : mode === 'weekdays' ? '1-5' : weekly.join(',')}` };
  const serialized = JSON.stringify(spec);
  useEffect(() => {
    if (!active) { onChange({ nextAt:initialNextAt,intervalSeconds:initialInterval === null ? null : initialInterval/1000,zone }); return; }
    let stopped = false; setPreview(null); setError(''); onChange(null);
    const timer = setTimeout(() => {
      const query = new URLSearchParams({ zone, schedule:serialized });
      void fetch(`/api/routine-schedule?${query}`).then(response => readJson<{ times: number[]; zone: string }>(response)).then(result => {
        if (stopped) return;
        setPreview(result);
        const schedule = JSON.parse(serialized) as Spec;
        onChange(schedule.kind === 'legacy_interval' ? { nextAt:initialNextAt,intervalSeconds:schedule.intervalMs/1000,zone:result.zone } : { schedule,zone:result.zone });
      }).catch(error => { if (!stopped) setError(error.message); });
    }, 300);
    return () => { stopped = true; clearTimeout(timer); };
  }, [active,zone,serialized,initialNextAt,initialInterval,onChange]);
  return <div className="routine-schedule k-stack" hidden={!active}>
    <Field label="Schedule"><Select value={mode} onChange={value => { setMode(value); if(value === 'interval' && mode === 'legacy') {setAmount('1');setUnit('hour');} }} disabled={disabled} options={[
      {value:'once',label:'Once'}, {value:'daily',label:'Every day'}, {value:'weekdays',label:'Weekdays'}, {value:'weekly',label:'Weekly'},
      {value:'interval',label:'Every N hours / minutes (advanced)'}, {value:'cron',label:'Custom cron (advanced)'}, ...(legacy && initialInterval ? [{value:'legacy',label:'Existing interval (minutes)'}] : []),
    ]}/></Field>
    <div className="k-grid-2">
      {mode === 'once' && <Field label="Date"><Input type="date" required={active} value={date} disabled={disabled} onChange={event=>setDate(event.target.value)}/></Field>}
      {['once','daily','weekdays','weekly'].includes(mode) && <Field label="Time"><Input type="time" required={active} value={clock} disabled={disabled} onChange={event=>setClock(event.target.value)}/></Field>}
      <Field label="Time zone" hint="IANA zone. Daily and weekly times follow its local clock."><Input value={zone} required={active} maxLength={100} disabled={disabled} onChange={event=>setZone(event.target.value)} placeholder="Asia/Jakarta"/></Field>
    </div>
    {mode === 'weekly' && <fieldset className="routine-days"><legend>Days</legend>{days.map((day,index)=><Checkbox key={day} label={day} checked={weekly.includes(String(index))} disabled={disabled} onChange={checked=>setWeekly(values=>checked ? [...values,String(index)].sort() : values.filter(value=>value!==String(index)))}/>)}</fieldset>}
    {(mode === 'interval' || mode === 'legacy') && <div className="k-grid-2"><Field label={mode === 'legacy' ? 'Every (minutes)' : 'Every'} hint={mode === 'legacy' ? 'Existing fixed interval; timing stays unchanged.' : intervalAnchor ? 'Elapsed time, starting at the selected slot.' : 'Elapsed time, up to 365 days. New intervals start after this duration.'}><Input type="number" required={active} min={mode === 'legacy' ? 1 : unit === 'minute' ? 5 : 1} max={unit === 'hour' && mode !== 'legacy' ? 8760 : 525600} step={mode === 'legacy' ? 'any' : 1} value={amount} disabled={disabled} onChange={event=>setAmount(event.target.value)}/></Field>{mode !== 'legacy' && <Field label="Unit"><Select value={unit} disabled={disabled} onChange={value=>setUnit(value as 'minute'|'hour'|'day')} options={[{value:'hour',label:'Hours'},{value:'minute',label:'Minutes'},...(unit === 'day' ? [{value:'day',label:'Days'}] : [])]}/></Field>}</div>}
    {mode === 'cron' && <Field label="Cron expression" hint="Five numeric fields: minute hour day month weekday (0 = Sunday). Minimum gap: 5 minutes."><Input value={expression} required={active} maxLength={256} disabled={disabled} placeholder="0 9 * * 1-5" onChange={event=>setExpression(event.target.value)}/></Field>}
    <section className="routine-preview" aria-label="Next runs" aria-live="polite"><strong>Next runs</strong>{error || zoneError ? <Alert tone="error">{error || zoneError}</Alert> : !preview ? <p role="status">Calculating schedule...</p> : preview.times.length ? <ol>{preview.times.map(at=><li key={at}><time dateTime={new Date(at).toISOString()}>{new Date(at).toLocaleString('en-GB',{timeZone:preview.zone,weekday:'short',day:'numeric',month:'short',year:'numeric',hour:'2-digit',minute:'2-digit'})}</time></li>)}</ol> : <p>No future runs. A past one-time schedule runs once when enabled.</p>}</section>
  </div>;
}

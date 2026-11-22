import { useCallback, useId, useRef, useState, type FormEvent } from 'react';
import { Square, Target } from 'lucide-react';
import { useVisiblePolling } from './hooks/useVisiblePolling';
import type { PanelRequest } from './RightPanel';
import { HttpError } from './http';
import { restoreModeCommand, type ModeCommand, type ModeDraft } from './goal-command';
import { Alert, Badge, Button, Dialog, Field, Input, Select, Textarea, useConfirm } from './ui/kit';
import './goal-mode.css';

type Goal = {
  id: string; target: string; current_turn: string; status: string; reason: string | null;
  turn_count: number; max_turns: number; max_minutes: number; tokens: number; max_tokens: number; estimated: boolean;
};
const status: Record<string, string> = {
  active: 'Working', judging: 'Judging', needs_you: 'Needs you', paused: 'Paused', met: 'Met',
  stopped: 'Stopped', token_limit: 'Token limit', wall_limit: 'Time limit', turn_limit: 'Turn limit',
  interrupted: 'Interrupted', failed: 'Failed', no_progress: 'No progress',
};
const currentGoal = (goal: Goal) => ['active', 'judging', 'needs_you', 'paused'].includes(goal.status);
const failureMessage = (reason: unknown) => reason instanceof Error ? reason.message : 'Request failed.';

export function GoalChip({ path, request, archived, refresh, onInspect, onStopped, onActiveTurn }: {
  path: string; request: PanelRequest; archived: boolean; refresh: number;
  onInspect: (turn: string) => void; onStopped: () => void; onActiveTurn: (turn: string) => void;
}) {
  const [goal, setGoal] = useState<Goal | null>(null);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const hasGoal = !!goal;
  const root = useVisiblePolling(useCallback(async current => {
    try {
      const value = await request<Goal | null>(`${path}/goal`);
      if (current()) { setGoal(value); onActiveTurn(value && currentGoal(value) ? value.current_turn : ''); }
    } catch (reason) {
      if (current() && hasGoal) setError(failureMessage(reason));
    }
  // The last good goal remains visible if a refresh fails.
  }, [path, request, refresh, hasGoal, onActiveTurn]), 1800);
  async function stop() {
    if (!goal || busy) return;
    setBusy(true); setError('');
    try {
      setGoal(await request<Goal>(`${path}/goal/stop`, { id: crypto.randomUUID(), goalId: goal.id }));
      onStopped();
    } catch (reason) { setError(failureMessage(reason)); }
    finally { setBusy(false); }
  }
  return <div ref={root} className="goal-mode">
    {goal && <div className="composer__note goal-chip" aria-label="Goal progress">
      <Target size={16} aria-hidden />
      <div className="goal-chip__main">
        <button type="button" className="k-link goal-chip__target" onClick={() => onInspect(goal.current_turn)} title={goal.target}>{goal.target}</button>
        <div className="goal-chip__limits" role="status">
          <Badge tone={goal.status === 'needs_you' ? 'warn' : goal.status === 'met' ? 'success' : undefined}>{status[goal.status] ?? goal.status}</Badge>
          <span>{goal.turn_count}/{goal.max_turns} turns</span><span>{goal.max_minutes} min</span>
          <span>{goal.tokens.toLocaleString()}/{goal.max_tokens.toLocaleString()} tokens{goal.estimated ? ' (est.)' : ''}</span>
        </div>
        {!currentGoal(goal) && goal.reason && <div className="goal-chip__reason">{goal.reason}</div>}
      </div>
      {currentGoal(goal) && !archived && <Button size="sm" variant="ghost" icon={<Square size={13} aria-hidden />} busy={busy} aria-label="Stop goal" onClick={() => void stop()}>Stop</Button>}
    </div>}
    {error && <Alert tone="error">{error}</Alert>}
  </div>;
}

export function ModeDialog({ path, storage, draft, recovery, invalid, request, members, recipient, files, onClose, onStarted }: {
  path: string; storage: string; draft: ModeDraft; recovery: ModeCommand | null; invalid: boolean; request: PanelRequest;
  members: { id: string; name: string }[]; recipient?: string; files: string[];
  onClose: () => void; onStarted: (mode: ModeDraft['mode']) => void;
}) {
  const id = useId();
  const confirm = useConfirm();
  const flight = useRef(false);
  const [pending, setPending] = useState(recovery);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [selected, setSelected] = useState(recovery?.body.recipient ?? recipient ?? '');
  const isGoal = draft.mode === 'goal';
  const previous = recovery?.mode === 'goal' ? recovery.body.target : recovery?.body.prompt;
  const locked = !!pending || busy;
  const label = isGoal ? 'Start goal' : 'Schedule loop';
  async function discard() {
    if (!await confirm({ title: 'Discard saved request?', description: 'Check goal progress or routines first. The request may have succeeded.', confirm: 'Discard', danger: true })) return;
    try { sessionStorage.removeItem(storage); onClose(); }
    catch (reason) { setError(failureMessage(reason)); }
  }
  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (flight.current || invalid) return;
    const form = new FormData(event.currentTarget);
    const common = { id: crypto.randomUUID(), ...(selected ? { recipient: selected } : {}) };
    const prompt = String(form.get('prompt')).trim();
    const until = String(form.get('until') ?? '').trim();
    const command: ModeCommand = pending ?? (isGoal ? {
      mode: 'goal', body: { ...common, target: prompt, maxTurns: Number(form.get('turns')), maxMinutes: Number(form.get('minutes')), maxTokens: Number(form.get('tokens')), ...(files.length ? { files } : {}) }
    } : {
      mode: 'loop', body: { ...common, prompt, intervalSeconds: Number(form.get('interval')) * 60, ...(until ? { until } : {}), maxRuns: Number(form.get('runs')) }
    });
    if (!pending && !prompt) { setError(isGoal ? 'Enter a goal target.' : 'Enter a loop prompt.'); return; }
    setBusy(true); setError(''); flight.current = true;
    try {
      restoreModeCommand(JSON.stringify(command));
      sessionStorage.setItem(storage, JSON.stringify(command));
      setPending(command);
      await request(`${path}/${command.mode}`, command.body);
      sessionStorage.removeItem(storage);
      onStarted(command.mode);
    } catch (reason) {
      if (reason instanceof HttpError && reason.status < 500) {
        sessionStorage.removeItem(storage);
        setPending(null);
      }
      setError(failureMessage(reason));
    } finally { flight.current = false; setBusy(false); }
  }

  return <Dialog open title={invalid ? 'Goal or loop recovery' : label} onOpenChange={open => { if (!open && !busy) onClose(); }} footer={<>
    {(pending || invalid) && <Button variant="ghost" disabled={busy} className="k-dialog__foot-start" onClick={() => void discard()}>Discard saved request</Button>}
    <Button disabled={busy} onClick={onClose}>Cancel</Button>
    {!invalid && <Button type="submit" form={id} variant="primary" busy={busy}>{pending && !busy ? 'Retry request' : label}</Button>}
  </>}>
    {error && <Alert tone="error">{error}</Alert>}
    {pending && !busy && <Alert tone="warn">Request is awaiting confirmation.</Alert>}
    {invalid ? <Alert tone="error">Saved request could not be read.</Alert> :
      <form id={id} aria-label={label} className="k-stack" onSubmit={submit}>
        {members.length > 0 && <Field label="Agent"><Select value={selected} onChange={setSelected} disabled={locked}
          options={[{ value: '', label: 'Default agent' }, ...members.map(member => ({ value: member.id, label: member.name }))]} /></Field>}
        <Field label={isGoal ? 'Target' : 'Prompt'}><Textarea name="prompt" required maxLength={12000} rows={3} defaultValue={previous ?? draft.text} readOnly={locked} /></Field>
        {isGoal ? <div className="goal-mode__limits">
          <Field label="Max turns"><Input name="turns" type="number" min={1} max={100} defaultValue={recovery?.mode === 'goal' ? recovery.body.maxTurns : 20} required readOnly={locked} /></Field>
          <Field label="Max minutes"><Input name="minutes" type="number" min={1} max={1440} defaultValue={recovery?.mode === 'goal' ? recovery.body.maxMinutes : 120} required readOnly={locked} /></Field>
          <Field label="Token budget"><Input name="tokens" type="number" min={1} max={10000000} defaultValue={recovery?.mode === 'goal' ? recovery.body.maxTokens : 250000} required readOnly={locked} /></Field>
        </div> : <>
          <div className="goal-mode__limits">
            <Field label="Interval (minutes)"><Input name="interval" type="number" min={1} max={525600} defaultValue={(recovery?.mode === 'loop' ? recovery.body.intervalSeconds : draft.intervalSeconds ?? 300) / 60} required readOnly={locked} /></Field>
            <Field label="Max runs"><Input name="runs" type="number" min={1} max={1000} defaultValue={recovery?.mode === 'loop' ? recovery.body.maxRuns : 20} required readOnly={locked} /></Field>
          </div>
          <Field label="Until" optional><Textarea name="until" maxLength={4000} rows={2} defaultValue={recovery?.mode === 'loop' ? recovery.body.until : ''} readOnly={locked} /></Field>
        </>}
      </form>}
  </Dialog>;
}

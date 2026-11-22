import { useCallback, useId, useState } from 'react';
import { ChevronRight } from 'lucide-react';
import type { PanelRequest } from './RightPanel';
import type { RunTarget } from './right-panel-state';
import { useVisiblePolling } from './hooks/useVisiblePolling';
import { Alert, Button, SkeletonRows } from './ui/kit';
import { mergeRunSteps, stepState } from './ui/transcript';

type Details = { started?: string | null; finished?: string | null; arguments?: string | null; result?: string | null };
type Tool = Details & { id: string; name: string; title?: string | null; state: string; after_action: number };
type NativeTool = Details & { id: string; name: string; state: string; failed?: number };
type ProviderCall = {
  seq: number; model: string; status: string; created: string; finished: string | null;
  duration_ms: number | null; input_tokens: number | null; output_tokens: number | null; total_tokens: number | null;
};
type Judgment = { turn: string; met: number; reason: string; number: number };
type Inspection = { status: string; error: string | null; judgments?: Judgment[]; tools: Tool[]; harness: NativeTool[]; providers: ProviderCall[]; engineStderr: string | null };
type Step = ReturnType<typeof mergeRunSteps<Tool, NativeTool>>[number];
const clock = new Intl.DateTimeFormat(undefined, { dateStyle: 'short', timeStyle: 'medium' });

function duration(ms: number | null) {
  return ms === null || !Number.isFinite(ms) ? null : ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(2)} s`;
}

function Time({ value }: { value: string | null | undefined }) {
  return value && Number.isFinite(Date.parse(value)) ? <time dateTime={value}>{clock.format(new Date(value))}</time> : 'Unavailable';
}

function InspectText({ label, text }: { label: string; text: string }) {
  const [expanded, setExpanded] = useState(false);
  const id = useId();
  const preview = text.slice(0, 640).split('\n').slice(0, 8).join('\n');
  const truncated = preview.length < text.length;
  return <div className="run-inspector__value">
    <div className="run-inspector__value-head"><strong>{label}</strong>{truncated && <button type="button" className="link-btn link-btn--small" aria-expanded={expanded} aria-controls={id} onClick={() => setExpanded(value => !value)}>{expanded ? 'Collapse' : 'Expand'}<span className="sr-only"> {label.toLowerCase()}</span></button>}</div>
    <pre id={id}><code>{expanded || !truncated ? text : `${preview}…`}</code></pre>
  </div>;
}

function StepDetails({ step, state, selected }: { step: Step; state: string; selected: RunTarget | null }) {
  const disclosure = useCallback((element: HTMLDetailsElement | null) => {
    if (!selected || !element) return;
    element.open = true;
    element.querySelector('summary')?.scrollIntoView({ block: 'nearest' });
    element.querySelector('summary')?.focus({ preventScroll: true });
  }, [selected]);
  const started = step.native?.started ?? step.tool?.started;
  const finished = step.native?.finished ?? step.tool?.finished;
  const elapsed = started && finished ? duration(Math.max(0, Date.parse(finished) - Date.parse(started))) : null;
  const args = step.native?.arguments ?? step.tool?.arguments;
  const result = step.native?.result ?? step.tool?.result;
  return <li><details ref={disclosure}>
    <summary><ChevronRight size={14} aria-hidden /><span>{step.title}</span><span className="run-inspector__state" data-state={state}>{state}</span></summary>
    <div className="run-inspector__details">
      <dl>
        <dt>Tool</dt><dd><code>{step.name}</code></dd>
        {step.native && <><dt>Native state</dt><dd><code>{step.native.state}</code></dd></>}
        <dt>Started</dt><dd><Time value={started} /></dd>
        <dt>Ended</dt><dd><Time value={finished} /></dd>
        <dt>Duration</dt><dd>{elapsed ?? 'Unavailable'}</dd>
      </dl>
      {args && <InspectText label="Arguments" text={args} />}
      {result && <InspectText label={state === 'Failed' ? 'Error' : 'Result'} text={result} />}
    </div>
  </details></li>;
}

function toolState(step: Step, status: string) {
  const failed = step.native?.failed || step.tool?.state === 'failed';
  return stepState(failed ? 'failed' : step.native?.state ?? step.tool?.state, status);
}

export function RunInspector({ target, request }: { target: RunTarget; request: PanelRequest }) {
  const [data, setData] = useState<Inspection | null>(null);
  const [error, setError] = useState('');
  const [retry, setRetry] = useState(0);
  const root = useVisiblePolling(useCallback(async current => {
    try {
      const result = await request<Inspection>(`/api/turns/${encodeURIComponent(target.turnId)}/inspect`);
      if (current()) { setData(result); setError(''); }
    } catch (reason) {
      if (current()) setError(reason instanceof Error ? reason.message : 'Run could not load.');
    }
  }, [target.turnId, request, retry]));
  const steps = data ? mergeRunSteps(data.tools, data.harness) : [];
  const selected = steps.find(step => step.id === target.stepId)
    ?? (!target.stepId && data ? steps.findLast(step => toolState(step, data.status) === 'Failed') : undefined);
  return <div className="right-panel__body run-inspector" ref={root}>
    {error && <Alert tone="error" actions={<Button size="sm" onClick={() => setRetry(value => value + 1)}>Retry</Button>}>{error}</Alert>}
    {!data && !error && <SkeletonRows count={3} />}
    {data && <>
      {data.error && <InspectText label="Run error" text={data.error} />}
      {!!data.judgments?.length && <section className="run-inspector__providers" aria-label="Goal judge">
        <h3>Goal judge</h3>
        <ul className="run-inspector__list">{data.judgments.map(judgment => <li key={judgment.turn}><details>
          <summary><ChevronRight size={14} aria-hidden /><span>Turn {judgment.number}</span><span className="run-inspector__state" data-state={judgment.met ? 'Done' : undefined}>{judgment.met ? 'Met' : 'More work'}</span></summary>
          <div className="run-inspector__details"><p>{judgment.reason}</p></div>
        </details></li>)}</ul>
      </section>}

      {steps.length > 0 && <ul className="run-inspector__list" aria-label="Steps">{steps.map(step => <StepDetails key={step.id} step={step} state={toolState(step, data.status)} selected={selected?.id === step.id ? target : null} />)}</ul>}
      {data.providers.length > 0 && <section className="run-inspector__providers" aria-labelledby="run-provider-calls">
        <h3 id="run-provider-calls">Provider calls</h3>
        <ul className="run-inspector__list">{data.providers.map(call => <li key={call.seq}><details>
          <summary><ChevronRight size={14} aria-hidden /><span>{call.model}</span><span className="run-inspector__state">{stepState(call.status)}{call.duration_ms !== null && ` · ${duration(call.duration_ms)}`}</span></summary>
          <div className="run-inspector__details"><dl>
            <dt>Started</dt><dd><Time value={call.created} /></dd>
            <dt>Ended</dt><dd><Time value={call.finished} /></dd>
            {call.input_tokens !== null && <><dt>Input tokens</dt><dd>{call.input_tokens.toLocaleString()}</dd></>}
            {call.output_tokens !== null && <><dt>Output tokens</dt><dd>{call.output_tokens.toLocaleString()}</dd></>}
            {call.total_tokens !== null && <><dt>Total tokens</dt><dd>{call.total_tokens.toLocaleString()}</dd></>}
          </dl></div>
        </details></li>)}</ul>
      </section>}
      {data.engineStderr && <InspectText label="Engine stderr" text={data.engineStderr} />}
      {!steps.length && !data.providers.length && !data.error && !data.engineStderr && !data.judgments?.length && <p role="status">No run details</p>}
    </>}
  </div>;
}

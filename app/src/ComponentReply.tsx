import { useId, useRef, useState, type FormEvent, type ReactNode } from 'react';
import { HtmlReplies } from './HtmlReply';
import type { HtmlReplyReference } from './html-reply';
import { Alert, Button, Card, Field, Input, useField } from './ui/kit';
import './component-reply.css';

type Scalar = string | number | boolean;
type Point = { label: string; value: number };
type TableData = { columns: string[]; rows: (Scalar | null)[][] };
type Metric = { value: string | number; detail?: string };
type TimelineItem = { label: string; when?: string; detail?: string };
type FormField = { name: string; label: string; required?: boolean } & (
  { type: 'text' | 'number' | 'boolean' } | { type: 'select'; options: string[] }
);
type Catalog = {
  bar: Point[]; line: Point[]; pie: Point[]; table: TableData; metric: Metric;
  timeline: TimelineItem[]; mermaid: { source: string; description: string }; form: FormField[];
};
type Node = { type: keyof Catalog; title: string; data: string; action?: string };
export type ComponentReplyData = {
  id: string;
  tree: { type: 'stack'; children: Node[] };
  data: Record<string, Catalog[keyof Catalog]>;
  actions: { id: string; type: 'submit'; label: string; context: Record<string, unknown> }[];
  frame: HtmlReplyReference | null;
};

function DataTable({ title, columns, rows, rowHeaders = false }: TableData & { title: string; rowHeaders?: boolean }) {
  const caption = useId();
  return <div className="component-table" role="region" aria-labelledby={caption} tabIndex={0}>
    <table>
      <caption id={caption}>{title}</caption>
      <thead><tr>{columns.map((column, i) => <th key={i} scope="col">{column}</th>)}</tr></thead>
      <tbody>{rows.map((row, i) => <tr key={i}>{row.map((cell, j) => {
        const value = cell === null ? '—' : String(cell);
        return rowHeaders && j === 0 ? <th key={j} scope="row">{value}</th> : <td key={j}>{value}</td>;
      })}</tr>)}</tbody>
    </table>
  </div>;
}

const numberLabel = new Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 2 });
function CartesianChart({ points, bar }: { points: Point[]; bar: boolean }) {
  const left = 54, right = 418, top = 10, bottom = 126;
  const low = Math.min(0, ...points.map(point => point.value));
  const high = Math.max(0, ...points.map(point => point.value));
  const max = high || low ? high : 1;
  const step = (right - left) / points.length;
  const x = (index: number) => left + (index + 0.5) * step;
  const y = (value: number) => bottom - (value - low) / (max - low) * (bottom - top);
  const ticks = [...new Set([low, (low + max) / 2, max, 0])].sort((a, b) => a - b);
  // Even spacing keeps small plots readable. The data table carries all labels.
  const labelIndexes = new Set(Array.from({ length: Math.min(5, points.length) }, (_, index) =>
    Math.round(index * (points.length - 1) / Math.max(1, Math.min(5, points.length) - 1))));
  const barWidth = Math.min(34, step * 0.58);
  return <svg className="component-cartesian" viewBox="0 0 426 152" aria-hidden="true">
    {ticks.map(tick => <g key={tick}>
      <line className={tick === 0 ? 'component-baseline' : 'component-gridline'} x1={left} x2={right} y1={y(tick)} y2={y(tick)} />
      {(tick === 0 || Math.abs(y(tick) - y(0)) > 13) && <text x={left - 7} y={y(tick)} dominantBaseline="middle" textAnchor="end">{numberLabel.format(tick)}</text>}
    </g>)}
    {bar ? points.map((point, i) => <rect key={i} className="component-plot-fill" x={x(i) - barWidth / 2} y={Math.min(y(0), y(point.value))} width={barWidth} height={Math.abs(y(point.value) - y(0))} rx={2} />) : <>
      <polyline className="component-plot-line" points={points.map((point, i) => `${x(i)},${y(point.value)}`).join(' ')} />
      {points.map((point, i) => <circle key={i} className="component-plot-fill" cx={x(i)} cy={y(point.value)} r={3} />)}
    </>}
    {points.map((point, i) => {
      if (!labelIndexes.has(i)) return null;
      const label = Array.from(point.label);
      return <text key={i} x={x(i)} y={146} textAnchor="middle">{label.length > 9 ? `${label.slice(0, 8).join('')}…` : point.label}</text>;
    })}
  </svg>;
}

function PieChart({ points }: { points: Point[] }) {
  const total = points.reduce((sum, point) => sum + point.value, 0);
  let start = 0;
  return <div className="component-pie" aria-hidden="true">
    <svg viewBox="0 0 100 100">
      {points.map((point, i) => {
        const length = point.value / total * 100;
        const offset = start;
        start += length;
        return length > 0 && <circle key={i} className={`component-palette-${i % 6}`} cx={50} cy={50} r={34} pathLength={100}
          strokeDasharray={`${length} ${100 - length}`} strokeDashoffset={-offset} transform="rotate(-90 50 50)" />;
      })}
    </svg>
    <ul>{points.map((point, i) => <li key={i}><span className={`component-palette-${i % 6}`} />{point.label}</li>)}</ul>
  </div>;
}

function Chart({ points, type, title }: { points: Point[]; type: 'bar' | 'line' | 'pie'; title: string }) {
  return <>{type === 'pie' ? <PieChart points={points} /> : <CartesianChart points={points} bar={type === 'bar'} />}
    <details className="component-chart-data">
      <summary>View data<span className="component-sr-only"> for {title}</span></summary>
      <DataTable title={`${title} data`} columns={['Label', 'Value']} rows={points.map(point => [point.label, point.value])} rowHeaders />
    </details>
  </>;
}

function ComponentReply({ reply, scope, form }: { reply: ComponentReplyData; scope: string; form?: (fields: FormField[], title: string) => ReactNode }) {
  const data = <K extends keyof Catalog>(node: Node, type: K) => {
    // Rust validates each data binding against the schema for its node type.
    return (node.type === type ? reply.data[node.data] : undefined) as Catalog[K];
  };
  function content(node: Node) {
    switch (node.type) {
      case 'bar': case 'line': case 'pie':
        return <Chart points={data(node, node.type)} type={node.type} title={node.title} />;
      case 'table': return <DataTable {...data(node, 'table')} title={node.title} />;
      case 'metric': {
        const metric = data(node, 'metric');
        return <><strong className="component-metric">{metric.value}</strong>{metric.detail && <p className="component-detail">{metric.detail}</p>}</>;
      }
      case 'timeline':
        return <ol className="component-timeline" role="list">{data(node, 'timeline').map((entry, i) => <li key={i}>
          <strong>{entry.label}</strong>{entry.when && <span>{entry.when}</span>}
          {entry.detail && <p className="component-detail">{entry.detail}</p>}
        </li>)}</ol>;
      case 'mermaid': return reply.frame && <HtmlReplies replies={[reply.frame]} scope={scope} />;
      case 'form': return form?.(data(node, 'form'), node.title);
      default: return null;
    }
  }
  return <Card className="component-reply">{reply.tree.children.map((node, i) =>
    <section className="component-node" key={i}>
      {node.type !== 'form' && node.type !== 'table' && <h3 className="k-section__title">{node.title}</h3>}
      {content(node)}
    </section>
  )}</Card>;
}

export function ComponentReplies({ replies, scope }: { replies?: ComponentReplyData[]; scope: string }) {
  return replies?.map(reply => <ComponentReply key={reply.id} reply={reply} scope={scope} />);
}

function FormSelect({ field }: { field: Extract<FormField, { type: 'select' }> }) {
  const input = useField();
  return <select id={input?.id} className="k-input" name={field.name} required={field.required} defaultValue="">
    <option value="">Select…</option>
    {field.options.map(option => <option key={option} value={option}>{option}</option>)}
  </select>;
}

type FormDecision = { decision: 'allow'; values: Record<string, Scalar> } | { decision: 'deny' };
type FormReceipt = { decision: 'allow' | 'deny' };
export function ComponentFormCard({ reply, revision, state, decision, path, scope, disabled, request }: {
  reply: ComponentReplyData; revision: number; state: string; decision: string | null; path: string; scope: string;
  disabled: boolean; request: <T>(path: string, data?: unknown) => Promise<T>;
}) {
  const titleId = useId();
  const titleRef = useRef<HTMLHeadingElement>(null);
  const pending = useRef<{ id: string; payload: string } | null>(null);
  const inFlight = useRef(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [receipt, setReceipt] = useState<FormReceipt | null>(null);
  const action = reply.actions[0];
  const open = state === 'approval' && !receipt;
  const outcome = state === 'expired' ? 'Form expired' : (receipt?.decision ?? decision) === 'deny' ? 'Form skipped' : 'Form submitted';
  async function decide(data: FormDecision) {
    if (disabled || !open || inFlight.current) return;
    const payload = JSON.stringify(data);
    const id = pending.current?.payload === payload ? pending.current.id : crypto.randomUUID();
    pending.current = { id, payload };
    inFlight.current = true;
    setBusy(true);
    setError('');
    try {
      const saved = await request<FormReceipt>(`${path}/harness-approvals/${reply.id}`, { id, revision, ...data });
      setReceipt(saved);
      titleRef.current?.focus();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : 'Could not submit this form. Try again.');
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  }
  function submit(event: FormEvent<HTMLFormElement>, fields: FormField[]) {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const values = Object.fromEntries(fields.flatMap<[string, Scalar]>(field => {
      if (field.type === 'boolean') return [[field.name, form.has(field.name)]];
      const raw = String(form.get(field.name) ?? '');
      const value = field.type === 'select' ? raw : raw.trim();
      if (!value) return [];
      return [[field.name, field.type === 'number' ? Number(value) : value]];
    }));
    void decide({ decision: 'allow', values });
  }
  return <ComponentReply reply={reply} scope={scope} form={(fields, title) => <>
    <h3 id={titleId} ref={titleRef} tabIndex={-1} className="k-section__title">{title}</h3>
    {!open ? <p className="component-detail" role="status">{outcome}</p> : <form className="k-stack component-form" aria-labelledby={titleId} method="post" action={`${path}/harness-approvals/${reply.id}`}
      autoComplete="off" onSubmit={event => submit(event, fields)}>
      <fieldset className="k-stack component-fields" disabled={disabled || busy}>
        <legend className="component-sr-only">{title}</legend>
        {fields.map(field => field.type === 'boolean' ?
          <label className="k-check component-checkbox" key={field.name}><input name={field.name} type="checkbox" required={field.required} />{field.label}{field.required && <span className="component-detail" aria-hidden="true">Required</span>}</label> :
          <Field label={field.label} optional={!field.required} key={field.name}>
            {field.type === 'select' ? <FormSelect field={field} /> : <Input name={field.name} type={field.type} required={field.required}
              {...(field.type === 'number' ? { step: 'any', min: -1e15, max: 1e15 } : { maxLength: 1000, pattern: '.*\\S.*' })} />}
          </Field>)}
      </fieldset>
      {error && <Alert tone="error">{error}</Alert>}
      {disabled && <p className="component-detail" role="status">Form unavailable</p>}
      <div className="k-row-actions">
        <Button type="button" size="sm" disabled={disabled || busy} onClick={() => void decide({ decision: 'deny' })}>Skip form</Button>
        <Button type="submit" size="sm" variant="primary" disabled={disabled} busy={busy}>{action.label}</Button>
      </div>
    </form>}
  </>} />;
}

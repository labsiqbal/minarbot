import { useCachedData, useQueryPolling } from './hooks/useCachedQuery';
import { queryCache } from './hooks/queryCache';
import { useEffect, useRef, useState } from 'react';
import './machines-data.css';
import { Alert, Button, Card, EmptyState, IconButton, PageHeader, Row, Section, SkeletonRows, Tabs, TabPanel, Tooltip } from './ui/kit';
import { ChevronLeft, ChevronRight, Receipt } from 'lucide-react';

type Call = { account?: string; family: string; seq: number; agent_name: string; model: string; origin: string; purpose: string; phase: string; status: string; created: string; input_tokens: number | null; output_tokens: number | null; total_tokens: number | null };
type Report = { transcription: { calls: number; pending: number; uncertain: number; cancelled: number; unknown: number; input_tokens: string | null; output_tokens: string | null; total_tokens: string | null; seconds: number | null }; summary: { calls: number; pending: number; failed: number; interrupted: number; input_tokens: string | null; output_tokens: string | null; total_tokens: string | null; unknown_input: number; unknown_output: number; unknown_total: number }; calls: Call[]; next_before: number | null };
const statuses: Record<string, string> = { pending: 'Waiting for response', received: 'Response received', failed: 'Failed or canceled', interrupted: 'Disconnected, result unknown' };
const amount = (value: number | string | null) => value === null ? '-' : BigInt(value).toLocaleString('en-US');
export function UsagePanel({ request }: { request: <T>(path: string) => Promise<T>; onClose: () => void }) {
  const [error, setError] = useState(''), [refresh, setRefresh] = useState(0);
  const [tab, setTab] = useState('calls');
  const [cursors, setCursors] = useState<number[]>([]), before = cursors.at(-1);
  const path = `/api/usage${before ? `?before=${before}` : ''}`;
  const { data: report } = useCachedData<Report>(path);
  const heading = useRef<HTMLHeadingElement>(null);
  useEffect(() => { heading.current?.focus(); }, [before]);
  useQueryPolling(async signal => {
    try { await queryCache.refresh(path, () => request<Report>(path)); if (!signal.aborted) setError(''); }
    catch (reason) { if (!signal.aborted) setError(reason instanceof Error ? reason.message : 'Usage could not load.'); }
  }, { interval: 5000 }, [before, request, refresh]);
  return <div className="k-page bs-root set4-page">
    <PageHeader headingRef={heading} title="Usage" />
    {error && <Alert tone="error" actions={<Button size="sm" onClick={() => setRefresh(value => value + 1)}>Retry</Button>}>{error}</Alert>}
    <Tabs value={tab} onChange={setTab} label="Usage" tabs={[{ value: 'calls', label: 'Calls' }, { value: 'transcription', label: 'Transcription' }]}>
      {!report ? !error && <SkeletonRows count={3} /> : <>
        <TabPanel value="calls">
          <Tooltip content={<span className="bs-root set4-tooltip">{"Provider-reported counts since tracking began. Totals may be incomplete; failed or disconnected calls may still be billed. Tool responses count per call, not per message."}</span>}><span className="k-field__hint" tabIndex={0}>Usage details</span></Tooltip>
          <Card className="set4-totals"><Row title="Calls" trailing={report.summary.calls.toLocaleString('en-US')} /><Row title="Waiting" trailing={report.summary.pending.toLocaleString('en-US')} /><Row title="Failed or canceled" trailing={report.summary.failed.toLocaleString('en-US')} /><Row title="Disconnected" trailing={report.summary.interrupted.toLocaleString('en-US')} /><Row title="Input tokens" trailing={amount(report.summary.input_tokens)} /><Row title="Output tokens" trailing={amount(report.summary.output_tokens)} /><Row title="Total tokens" trailing={amount(report.summary.total_tokens)} /></Card>
        <Section title="Call history">
          {!report.calls.length ? <EmptyState icon={<Receipt size={18} />} title="No calls" /> : <div className="k-table-wrap"><table className="k-table usage-calls"><colgroup><col style={{ width: 'var(--usage-agent-width, 240px)' }} /><col style={{ width: 220 }} /><col style={{ width: 160 }} /><col style={{ width: 'var(--usage-status-width, 236px)' }} /><col style={{ width: 88 }} /><col style={{ width: 88 }} /><col style={{ width: 88 }} /></colgroup><thead><tr><th>Agent and model</th><th>Provider</th><th>Purpose</th><th>Status</th><th className="is-num">Input</th><th className="is-num">Output</th><th className="is-num">Total</th></tr></thead><tbody>{report.calls.map(call => { const purpose = call.phase === 'image' ? 'Image generation' : call.phase === 'compaction' ? 'History summary' : call.purpose === 'routing' ? 'Channel recipient selection' : 'Agent or workflow reply'; const provider = <>{call.origin === 'local-harness' ? call.model : call.family === 'codex' ? 'Codex app-server' : call.family === 'claude' ? 'Claude Code' : call.family === 'openai-images' ? 'OpenAI Images-compatible' : call.family === 'gemini' ? 'Gemini GenerateContent' : call.family === 'anthropic' ? 'Anthropic Messages' : 'OpenAI-compatible'}<br /><span className="k-field__hint" title={call.origin} style={{ display: 'block', overflow: 'hidden', textOverflow: 'ellipsis' }}>{call.origin}</span></>; return <tr key={call.seq}><td style={{ overflowWrap: 'break-word' }}><strong>{call.agent_name}</strong><br /><span className="k-field__hint">{call.model}<br />{call.phase === 'image' ? 'Separate image provider' : call.account || 'Server default'}<br />{new Date(call.created).toLocaleString('en-US')}</span><div className="usage-call__compact k-field__hint">{provider}<details><summary>Details</summary><div>{purpose}</div><div>Input {amount(call.input_tokens)} · Output {amount(call.output_tokens)} · Total {amount(call.total_tokens)}</div></details></div></td><td>{provider}</td><td style={{ overflowWrap: 'normal' }}>{purpose}</td><td>{statuses[call.status]}</td><td className="is-num" style={{ whiteSpace: 'nowrap' }}>{amount(call.input_tokens)}</td><td className="is-num" style={{ whiteSpace: 'nowrap' }}>{amount(call.output_tokens)}</td><td className="is-num" style={{ whiteSpace: 'nowrap' }}>{amount(call.total_tokens)}</td></tr>; })}</tbody></table></div>}
          {(cursors.length > 0 || !!report.next_before) && <div className="k-pager"><IconButton label="Newer calls" disabled={!cursors.length} onClick={() => setCursors(value => value.slice(0, -1))}><ChevronLeft size={16} aria-hidden /></IconButton><IconButton label="Older calls" disabled={!report.next_before} onClick={() => setCursors(value => [...value, report.next_before!])}><ChevronRight size={16} aria-hidden /></IconButton></div>}
        </Section>
        </TabPanel>
        <TabPanel value="transcription">
          <Tooltip content={<span className="bs-root set4-tooltip">{"Provider-reported duration and tokens; missing values remain unknown. Latest attempt details are in the source file."}</span>}><span className="k-field__hint" tabIndex={0}>Usage details</span></Tooltip>
          <Card className="set4-totals"><Row title="Calls" trailing={report.transcription.calls} /><Row title="Pending" trailing={report.transcription.pending} /><Row title="Uncertain" trailing={report.transcription.uncertain} /><Row title="Cancelled" trailing={report.transcription.cancelled} /><Row title="Usage unknown" trailing={report.transcription.unknown} /><Row title="Input tokens" trailing={amount(report.transcription.input_tokens)} /><Row title="Output tokens" trailing={amount(report.transcription.output_tokens)} /><Row title="Total tokens" trailing={amount(report.transcription.total_tokens)} /><Row title="Duration" trailing={report.transcription.seconds === null ? '-' : `${report.transcription.seconds.toLocaleString('en-US')} seconds`} /></Card>
        </TabPanel>
      </>}
    </Tabs>
  </div>;
}

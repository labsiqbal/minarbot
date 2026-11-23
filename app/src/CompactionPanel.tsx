import { useVisiblePolling } from './hooks/useVisiblePolling';
import { useCallback, useEffect, useRef, useState } from 'react';
import { ChevronLeft, ChevronRight, History } from 'lucide-react';
import { Alert, Badge, Button, Card, CodeBlock, Collapsible, Dialog, IconButton, Row } from './ui/kit';
import './MemoryPanel.css';
type Summary = { version: string; through: number; summary: string; source_count: number; sources: { id: string; seq: number; status: string }[]; next_after: number | null };
export function CompactionPanel({ path, request }: { path: string; request: <T>(path: string) => Promise<T> }) {
  const [open, setOpen] = useState(false), [data, setData] = useState<Summary | null>(null), [loaded, setLoaded] = useState(false), [error, setError] = useState('');
  const [pages, setPages] = useState<number[]>([]), [refresh, setRefresh] = useState(0), after = pages.at(-1) ?? 0;
  const heading = useRef<HTMLHeadingElement>(null), summaryButton = useRef<HTMLButtonElement>(null), focusPage = useRef(false);
  useEffect(() => { if (loaded && focusPage.current) { heading.current?.focus(); focusPage.current = false; } }, [loaded, data]);
  const root = useVisiblePolling(useCallback(async current => {
    try { const value = await request<Summary | null>(`${path}?after=${after}`); if (current()) { setData(value); setLoaded(true); setError(''); } }
    catch (error) { if (current()) setError(error instanceof Error ? error.message : 'Summary could not load.'); }
  }, [after, refresh, path, request]));
  return <div ref={root} className="bs-root summary-row">
    <div className="details-list-row">
      {data ? <button ref={summaryButton} type="button" className="panel-row panel-row--link" onClick={() => setOpen(true)}><History size={16} aria-hidden /><span className="panel-row__title">History summary</span><Badge>Up to date</Badge><ChevronRight size={14} aria-hidden /></button> : <div className="panel-row"><History size={16} aria-hidden /><span className="panel-row__title">History summary</span><Badge>{error ? 'Unavailable' : loaded ? 'None yet' : 'Loading'}</Badge></div>}

    </div>
    {error && <Alert tone="error" actions={<Button size="sm" onClick={() => setRefresh(n => n + 1)}>Retry</Button>}>{error}</Alert>}
    <Dialog open={open} onOpenChange={next => { setOpen(next); if (!next) requestAnimationFrame(() => summaryButton.current?.focus()); }} title="History summary" size="lg">
      {error ? <Alert tone="error">{error}</Alert> : !loaded ? <p role="status">Loading summary...</p> : data && <>
        <Badge>{data.source_count} source messages · through {data.through}</Badge>
        <CodeBlock label="History summary">{data.summary}</CodeBlock>
        <Collapsible title="Source messages" defaultOpen>
          <h2 ref={heading} tabIndex={-1} className="k-section__title">Source list</h2>
          <Card>{data.sources.map(source => <Row key={source.id} title={<>Order {source.seq} · {source.status}</>} meta={<code style={{ overflowWrap: 'anywhere' }}>{source.id}</code>} />)}</Card>
          {(pages.length > 0 || data.next_after) && <div className="k-pager">
            <IconButton label="Previous sources" disabled={!pages.length} onClick={() => { focusPage.current = true; setPages(v => v.slice(0, -1)); }}><ChevronLeft size={16} aria-hidden /></IconButton>
            <IconButton label="Next sources" disabled={!data.next_after} onClick={() => { focusPage.current = true; setPages(v => [...v, data.next_after!]); }}><ChevronRight size={16} aria-hidden /></IconButton>
          </div>}
        </Collapsible>
      </>}
    </Dialog></div>;
}

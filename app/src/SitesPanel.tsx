import { useCachedData, useQueryVersion } from './hooks/useCachedQuery';
import { queryCache } from './hooks/queryCache';
import { useVisiblePolling } from './hooks/useVisiblePolling';
import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react';
import { siteCommand } from './site-command';
import { Alert, Button, Card, Dialog, EmptyState, Field, IconButton, Input, Row, SkeletonRows, Textarea, Tooltip, useConfirm, RecoveryAlert } from './ui/kit';
import { ChevronLeft, ChevronRight, ExternalLink, Plus } from 'lucide-react';

type Site = { id: string; name: string; source: string; revision: number; deleted: number };
type Page = { sites: Site[]; next_after: string | null };
type Command = ReturnType<typeof siteCommand>;
export function SitesPanel({ agent, request }: { agent: string; request: <T>(path: string, data?: unknown) => Promise<T> }) {
  const path = `/api/agents/${agent}/sites`, key = `minarbot-sites:${agent}`;
  const [open, setOpen] = useState(true), [editorOpen, setEditorOpen] = useState(false), [cursors, setCursors] = useState(['']);
  const [editing, setEditing] = useState<Site | null>(null), [name, setName] = useState(''), [source, setSource] = useState('');
  const [error, setError] = useState(''), [busy, setBusy] = useState(false), [pending, setPending] = useState<Command | null>(null), [invalid, setInvalid] = useState(false);
  const [reload, setReload] = useState(0), [notice, setNotice] = useState('');
  const generation = useRef(0), heading = useRef<HTMLHeadingElement>(null), nameInput = useRef<HTMLInputElement>(null), confirm = useConfirm();
  const editorTrigger = useRef<HTMLElement | null>(null), editorWasOpen = useRef(false), addButton = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (!editorOpen && editorWasOpen.current) requestAnimationFrame(() => (editorTrigger.current?.isConnected ? editorTrigger.current : addButton.current)?.focus());
    editorWasOpen.current = editorOpen;
  }, [editorOpen]);
  const after = cursors.at(-1)!;
  const failure = (reason: unknown) => reason instanceof TypeError ? 'Connection lost. Retry with the same request.' : reason instanceof Error ? reason.message : 'Site could not load. Try again.';
  useEffect(() => {
    try { const stored = sessionStorage.getItem(key); if (stored) { setPending(siteCommand(JSON.parse(stored))); setOpen(true); setEditorOpen(true); } }
    catch { setInvalid(true); setOpen(true); setEditorOpen(true); }
    return () => { generation.current++; };
  }, [key]);
  const [loadError, setLoadError] = useState('');
  const queryPath = `${path}?after=${after}`;
  const { data: page } = useCachedData<Page>(queryPath);
  const version = useQueryVersion();
  const root = useVisiblePolling(useCallback(async current => {
    if (!open || busy) return;
    try { await queryCache.refresh(queryPath, () => request<Page>(queryPath)); if (current()) setLoadError(''); }
    catch (reason) { if (current()) setLoadError(failure(reason)); }
  }, [open, queryPath, reload, request, busy, version]));
  useEffect(() => { heading.current?.focus(); }, [path, after]);
  useEffect(() => { if (editing && !busy && !pending) nameInput.current?.focus(); }, [editing, busy, pending]);
  function reset() { generation.current++; setEditing(null); setName(''); setSource(''); setError(''); nameInput.current?.focus(); }
  async function edit(id: string) {
    editorTrigger.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const version = ++generation.current; setBusy(true); setError('');
    try { const value = await request<Site>(`${path}/${id}`); if (generation.current !== version) return; if (value.deleted) throw new Error('Site has been deleted. Reload the list.'); setEditing(value); setName(value.name); setSource(value.source); setEditorOpen(true); }
    catch (reason) { if (generation.current === version) setError(failure(reason)); }
    finally { if (generation.current === version) setBusy(false); }
  }
  async function save(event?: FormEvent, deleting = false) {
    event?.preventDefault(); if (busy || invalid) return;
    if (deleting && !pending && !await confirm({ title: `Delete ${editing?.name ?? 'site'}?`, description: 'This permanently deletes the site. Its link will stop working.', confirm: 'Delete site', danger: true })) return;
    const version = generation.current; setError(''); setNotice(''); setBusy(true);
    try {
      const command = pending ?? siteCommand({ id: crypto.randomUUID(), ...(editing ? { site: editing.id, revision: editing.revision } : {}), ...(deleting ? { delete: true } : { name, source }) });
      sessionStorage.setItem(key, JSON.stringify(command)); setPending(command);
      const result = await request<Site>(path, command);
      if (sessionStorage.getItem(key) === JSON.stringify(command)) sessionStorage.removeItem(key);
      if (version !== generation.current) return;
      setPending(null); reset(); setNotice(result.deleted ? 'Site deleted.' : 'Site saved.'); setReload(value => value + 1);
    } catch (reason) { if (version === generation.current) setError(failure(reason)); }
    finally { if (version === generation.current || version + 1 === generation.current) setBusy(false); }
  }
  async function discard() {
    if (!await confirm({ title: 'Discard site recovery?', description: 'Changes may already be saved. Check the site list before making new changes.', confirm: 'Discard recovery', danger: true })) return;
    try { sessionStorage.removeItem(key); setPending(null); setInvalid(false); reset(); setReload(value => value + 1); }
    catch { setError('Recovery could not be cleared. Check browser storage.'); }
  }
  const locked = busy || !!pending || invalid;
  const pager = (back: boolean, next: boolean) => (back || next) && <div className="k-pager"><IconButton label="Previous sites" disabled={!back} onClick={() => setCursors(values => values.slice(0, -1))}><ChevronLeft size={16} aria-hidden /></IconButton><IconButton label="Next sites" disabled={!next} onClick={() => setCursors(values => [...values, page!.next_after!])}><ChevronRight size={16} aria-hidden /></IconButton></div>;
  return <div ref={root} className="k-stack bs-root">
    <header className="k-section__head"><h2 ref={heading} tabIndex={-1} className="k-section__title">Agent sites</h2><div className="k-section__actions"><div className="k-inline"><Button ref={addButton} size="sm" variant="primary" icon={<Plus size={14} aria-hidden />} disabled={locked} onClick={event => { editorTrigger.current = event.currentTarget; setEditorOpen(true); }}>{editing ? 'Continue editing' : 'Create site'}</Button></div></div></header>
    {loadError && <Alert tone="error" actions={<Button size="sm" onClick={() => setReload(value => value + 1)}>Retry</Button>}>{loadError}</Alert>}
    {open && <div className="k-stack">
      {error && <Alert tone="error">{error}</Alert>}{notice && <span className="sr-only" role="status">{notice}</span>}
      {(pending || invalid) && <RecoveryAlert inFlight={busy} tone={invalid ? 'error' : 'warn'} title={invalid ? 'Recovery data could not be read' : 'Changes are not confirmed yet'} actions={<>{pending && <Button size="sm" variant="primary" busy={busy} onClick={() => void save()}>Try again</Button>}<Button size="sm" disabled={busy} onClick={() => void discard()}>Discard recovery</Button></>}>{invalid ? 'Check the site list before discarding it.' : 'Retrying uses the same contents and ID.'}</RecoveryAlert>}
      {!page ? <><span className="sr-only" role="status">Loading sites...</span><SkeletonRows /></> : !page.sites.length ? <EmptyState title="No sites" icon={<ExternalLink size={18} />} /> : <Card>{page.sites.map(site => <Row key={site.id} title={site.name} leading={<ExternalLink size={16} aria-hidden />} meta={<a href={`/sites/${site.id}`} target="_blank" rel="noopener noreferrer">Open {site.name}</a>} trailing={<Button size="sm" disabled={locked} onClick={() => void edit(site.id)} aria-label={`Edit ${site.name}`}>Edit</Button>} />)}</Card>}
      {pager(cursors.length > 1, !!page?.next_after)}
    </div>}
    <Dialog open={editorOpen} size="md" title={editing ? 'Replace site contents' : 'New site'} onOpenChange={setEditorOpen} footer={<><Button onClick={() => setEditorOpen(false)}>Close</Button><Button type="submit" form="site-editor" variant="primary" busy={busy} disabled={locked}>{busy ? 'Saving...' : editing ? 'Save changes' : 'Create site'}</Button></>}>
      {error && <Alert tone="error">{error}</Alert>}{notice && <span className="sr-only" role="status">{notice}</span>}
      {(pending || invalid) && <RecoveryAlert inFlight={busy} tone={invalid ? 'error' : 'warn'} title={invalid ? 'Recovery data could not be read' : 'Changes are not confirmed yet'} actions={<>{pending && <Button size="sm" variant="primary" busy={busy} onClick={() => void save()}>Try again</Button>}<Button size="sm" disabled={busy} onClick={() => void discard()}>Discard recovery</Button></>}>{invalid ? 'Check the site list before discarding it.' : 'Retrying uses the same contents and ID.'}</RecoveryAlert>}
      <form id="site-editor" className="k-stack" onSubmit={event => void save(event)}>
        <Field label="Name"><Input ref={nameInput} required maxLength={80} value={name} disabled={locked} onChange={event => setName(event.target.value)} /></Field>
        <Field label="HTML"><Tooltip content="Text, tables, and inline styles only. Scripts, forms, images, and external connections are removed. Sites stay on this server."><Textarea required rows={8} maxLength={262144} value={source} disabled={locked} onChange={event => setSource(event.target.value)} /></Tooltip></Field>
        {editing && <><Button size="sm" disabled={locked} onClick={reset}>Cancel edit</Button><Button type="button" variant="danger" disabled={locked} onClick={() => void save(undefined, true)}>Delete site</Button></>}
      </form>
    </Dialog>
  </div>;
}

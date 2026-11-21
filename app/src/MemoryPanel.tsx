import { useCachedData, useQueryVersion } from './hooks/useCachedQuery';
import { queryCache } from './hooks/queryCache';
import { useVisiblePolling } from './hooks/useVisiblePolling';
import { useCallback, useEffect, useRef, useState } from 'react';
import { memoryCommand, memoryDraft, type MemoryCommand, type MemoryDraft, type MemoryEntry } from './memory-command.ts';

import { ChevronLeft, ChevronRight, Ellipsis } from 'lucide-react';
import { ActionMenu, Alert, Button, Card, Checkbox, EmptyState, Field, IconButton, Input, PageHeader, Row, Section, SkeletonRows, Textarea, useConfirm, RecoveryAlert } from './ui/kit';

type Entry = MemoryEntry;
type Page = { items: Entry[]; next_after: string | null };
export function MemoryPanel({ path, channel, archived, request, onClose }: { path: string; channel: boolean; archived: boolean; request: <T>(path: string, data?: unknown) => Promise<T>; onClose: () => void }) {
  const confirm = useConfirm();
  const key = `minarbot:memory:${path}`;
  const draftKey = `${key}:draft`;
  const [recovery] = useState(() => {
    try { const raw = sessionStorage.getItem(key), draft = sessionStorage.getItem(draftKey); return { pending: raw ? memoryCommand(JSON.parse(raw)) : null, draft: draft ? memoryDraft(JSON.parse(draft)) : null, error: '' }; }
    catch { return { pending: null, draft: null, error: 'Saved changes could not be read. Discard recovery to continue.' }; }
  });
  const [pending, setPending] = useState<MemoryCommand | null>(recovery.pending), [blocked, setBlocked] = useState(recovery.error);
  const [error, setError] = useState(''), [notice, setNotice] = useState('');
  const [query, setQuery] = useState(''), [search, setSearch] = useState(''), [cursors, setCursors] = useState<string[]>([]), [refresh, setRefresh] = useState(0);
  const [entry, setEntry] = useState<Entry | null>(recovery.draft?.entry ?? null), [text, setText] = useState(recovery.draft?.text ?? ''), [essential, setEssential] = useState(recovery.draft?.essential ?? false), [busy, setBusy] = useState(false);
  const [automatic, setAutomatic] = useState(recovery.draft?.automatic ?? false);
  const mounted = useRef(false), generation = useRef(0), sending = useRef(false), heading = useRef<HTMLHeadingElement>(null), editor = useRef<HTMLTextAreaElement>(null);
  const [draftStored, setDraftStored] = useState(true);
  const after = cursors.at(-1) ?? '', locked = !!pending || !!blocked || busy || archived;
  const dirty = text !== (entry?.text ?? '') || essential !== !!entry?.essential || automatic !== (entry?.selection_locked === 0);
  useEffect(() => { mounted.current = true; heading.current?.focus(); return () => { mounted.current = false; generation.current++; }; }, []);
  const [loadError, setLoadError] = useState('');
  const queryPath = `${path}?${new URLSearchParams({ query: search, after })}`;
  const { data: page, fresh } = useCachedData<Page>(queryPath);
  const version = useQueryVersion();
  const root = useVisiblePolling(useCallback(async current => {
    if (busy) return;
    try { await queryCache.refresh(queryPath, () => request<Page>(queryPath)); if (current()) setLoadError(''); }
    catch (error) { if (current()) setLoadError(error instanceof Error ? error.message : 'Memory could not load.'); }
  }, [queryPath, refresh, request, busy, version]));
  useEffect(() => { generation.current++; }, [path, after, search]);
  function draft(value: MemoryDraft) {
    try { sessionStorage.setItem(draftKey, JSON.stringify(value)); setDraftStored(true); }
    catch { setDraftStored(false); setError('Draft has not been saved in the browser. Copy the text before leaving this page.'); }
    setEntry(value.entry); setText(value.text); setEssential(value.essential); setAutomatic(value.automatic ?? false);
  }
  function clear() { sessionStorage.removeItem(draftKey); setEntry(null); setText(''); setEssential(false); setAutomatic(false); }
  async function choose(value: Entry | null) {
    if (locked || (value && !fresh) || dirty && !await confirm({ title: 'Discard unsaved note changes?', confirm: 'Discard changes', danger: true })) return;
    draft({ entry: value, text: value?.text ?? '', essential: !!value?.essential, automatic: value?.selection_locked === 0 }); editor.current?.focus();
  }
  async function discard() {
    if (!await confirm({ title: 'Delete recovery data?', description: 'Changes may already be saved on the server. Check the list before making new changes.', confirm: 'Delete recovery', danger: true })) return;
    try { sessionStorage.removeItem(key); setPending(null); setBlocked(''); setError(''); setNotice('Draft kept. For revision conflicts, choose Edit note from the latest list.'); if (blocked) clear(); setRefresh(n => n + 1); }
    catch { setError('Recovery data could not be deleted. Check browser storage.'); }
  }
  async function save(remove = false) {
    if (sending.current || archived && !pending || blocked) return;
    if (!pending && remove && (!entry || !await confirm({ title: 'Delete this memory note?', description: 'Conversation history and backups still retain earlier copies.', confirm: 'Delete note', danger: true }))) return;
    let command;
    try {
      command = pending ?? memoryCommand({ id: crypto.randomUUID(), entry: entry?.id ?? crypto.randomUUID(), revision: entry?.revision ?? 0, text: remove ? '' : text, essential: remove ? false : essential, remove, automatic });
      sessionStorage.setItem(key, JSON.stringify(command));
    } catch (error) { setError(error instanceof Error ? error.message : 'Changes could not be saved in the browser.'); return; }
    sending.current = true; generation.current++; setPending(command); setBusy(true); setError(''); setNotice('');
    try {
      await request(path, command);
      if (!mounted.current) return;
      clear(); sessionStorage.removeItem(key); setPending(null); setNotice('Changes saved.'); setRefresh(n => n + 1); heading.current?.focus();
    } catch (error) {
      if (mounted.current && error && typeof error === 'object' && 'status' in error && [400, 413].includes(Number(error.status))) {
        try { sessionStorage.removeItem(key); setPending(null); } catch { /* Keep exact retry if recovery storage cannot be cleared. */ }
      }
      if (mounted.current) setError(error instanceof TypeError ? 'Save result is unknown. Retry with the same changes.' : error instanceof Error ? error.message : 'Changes are not confirmed yet. Try again.');
    } finally { sending.current = false; if (mounted.current) setBusy(false); }
  }
  return <div ref={root} className="k-page bs-root">
    <PageHeader headingRef={heading} title={`Memory for this ${channel ? 'section' : 'agent'}`}
      description={channel ? 'Used by members when replying in this section. Private agent memory is excluded.' : 'Used by the agent in Main chat and all its conversations. Excluded from sections.'}
      actions={<><Button variant="ghost" onClick={async () => { if (draftStored || await confirm({ title: 'Leave and discard changes?', description: 'Draft has not been saved in the browser.', confirm: 'Leave', danger: true })) onClose(); }}>Back</Button></>} />
    <p className="k-field__hint" style={{ margin: 0 }}>Notes are sent to the provider when included or searched. Agents can add or edit note contents, but cannot delete notes. Context selection stays under your control unless delegated to the agent. Saved changes remain even if the task is canceled.</p>
    {archived && <Alert>Section archived. Restore the section to change memory.</Alert>}
    {(blocked || error) && <Alert tone="error">{blocked || error}</Alert>}{notice && <Alert>{notice}</Alert>}
    {(blocked || pending) && <RecoveryAlert inFlight={busy} tone="warn" title="Changes await confirmation" actions={<>{pending && <Button size="sm" disabled={busy} onClick={() => void save()}>Retry save</Button>}<Button size="sm" disabled={busy} onClick={() => void discard()}>Discard recovery and reload</Button></>}>
      Retrying uses the same data.{pending && <p style={{ whiteSpace: 'pre-wrap' }}>{pending.remove ? 'Delete note' : pending.text}</p>}
    </RecoveryAlert>}
    <Section title={entry ? 'Edit note' : 'New note'}>
      <form className="k-stack" onSubmit={event => { event.preventDefault(); if (!locked) void save(); }}>
        <Field label="Memory contents"><Textarea ref={editor} maxLength={2000} required value={text} disabled={locked} onChange={event => draft({ entry, text: event.target.value, essential, automatic })} /></Field>
        <Checkbox checked={essential} disabled={locked} onChange={checked => draft({ entry, text, essential: checked, automatic })} label="Always include in context" />
        <Checkbox checked={automatic} disabled={locked} onChange={checked => draft({ entry, text, essential, automatic: checked })} label="Allow agent to manage context selection" />
        <p className="k-field__hint" style={{ margin: 0 }}>When enabled, the agent may change context selection for future runs. Excluded notes can still be found by search. Maximum 2,000 characters per note, 128 notes, and 8 KiB of combined context.</p>
        <div className="k-row-actions">
          <Button type="submit" variant="primary" disabled={locked || !text.trim()}>Save note</Button>
          <Button disabled={locked} onClick={() => void choose(null)}>New note</Button>
          {entry && <ActionMenu label="Note actions" trigger={<button type="button" className="k-icon-btn" aria-label="Note actions" disabled={locked}><Ellipsis size={16} aria-hidden /></button>} items={[{ label: 'Delete note', danger: true, disabled: locked, onSelect: () => void save(true) }]} />}
        </div>
      </form>
    </Section>
    {loadError && <Alert tone="error" actions={<Button size="sm" onClick={() => setRefresh(n => n + 1)}>Retry</Button>}>{loadError}</Alert>}
    <Section title="Saved notes">
      <form className="k-stack" onSubmit={event => { event.preventDefault(); setCursors([]); setSearch(query); setRefresh(n => n + 1); }}>
        <Field label="Search memory"><Input value={query} maxLength={200} disabled={busy} onChange={event => setQuery(event.target.value)} /></Field>
        <div className="k-row-actions"><Button type="submit" disabled={busy}>Search</Button></div>
      </form>
      {!page ? !error && <><span className="sr-only" role="status">Loading memory...</span><SkeletonRows /></> : <>
        {page.items.length ? <Card>{page.items.map(row => <Row key={row.id}
          title={<span style={{ display: 'block', whiteSpace: 'pre-wrap', overflowWrap: 'anywhere' }}>{row.text}</span>}
          meta={<>{row.essential ? 'Always included' : 'Searched when needed'} · {row.selection_locked === 0 ? 'Selection managed by agent' : 'Selection locked by user'}</>}
          trailing={<Button size="sm" disabled={locked || !fresh} onClick={() => void choose(row)}>Edit note</Button>} />)}</Card> : <EmptyState title="No matching notes yet." />}
        {(cursors.length > 0 || page.next_after) && <div className="k-pager">
          <IconButton label="Previous page" disabled={busy || !cursors.length} onClick={() => setCursors(v => v.slice(0, -1))}><ChevronLeft size={16} aria-hidden /></IconButton>
          <IconButton label="Next page" disabled={busy || !page.next_after} onClick={() => setCursors(v => [...v, page.next_after!])}><ChevronRight size={16} aria-hidden /></IconButton>
        </div>}
      </>}
    </Section>
  </div>;
}

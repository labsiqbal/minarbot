import { useCallback, useRef, useState } from 'react';
import { ChevronLeft, ChevronRight, Ellipsis } from 'lucide-react';
import { useCachedData } from './hooks/useCachedQuery';
import { useVisiblePolling } from './hooks/useVisiblePolling';
import { queryCache } from './hooks/queryCache';
import { ActionMenu, Alert, Badge, Button, Card, Dialog, EmptyState, Field, IconButton, Input, Row, Select, SkeletonRows, Textarea } from './ui/kit';

type Source = { kind?: string; id?: string; agent?: string; channel?: string | null; thread?: string | null; role?: string; created?: string };
type Knowledge = { id: number; text: string; kind: string; sources: Source[]; revision: number; essential: boolean };
type Question = { id: string; question: string; revision: number; pending: boolean; reflection?: { answer: string; evidence: { id: number; text: string; source: Source[] }[] } };
type RecordPage = { items: (Knowledge | Question)[]; next_after?: number | null };
type UnitChange = { unit: number; revision: number } & (
  { action: 'pin'; essential: boolean } | { action: 'edit'; text: string } | { action: 'forget' | 'restore' }
);
type QuestionChange = { model: string; revision: number } & (
  { action: 'save_question'; question: string } | { action: 'forget_question' | 'restore_question' }
);
type Change = (UnitChange | QuestionChange) & { id: string };
type Editor = { item?: Knowledge | Question; text: string; model: string };

const uuid = /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i;
function Sources({ sources }: { sources: Source[] }) {
  const unique = sources.filter((source, i) => i === sources.findIndex(other => other.id === source.id && other.role === source.role));
  return unique.length > 0 && <details className="memory-sources">
    <summary>Sources ({unique.length})</summary>
    <ul>{unique.map((source, index) => {
      const href = source.channel && uuid.test(source.channel) ? `/rooms/${source.channel}`
        : source.agent && uuid.test(source.agent) ? `/agents/${source.agent}${source.thread && uuid.test(source.thread) ? `/conversations/${source.thread}` : ''}` : undefined;
      const label = source.role === 'prompt' ? 'User message' : source.role === 'reply' ? 'Agent reply' : source.kind === 'note' ? 'Saved note' : 'Retained memory';
      return <li key={index}>{href ? <a className="k-link" href={href}>{label}</a> : label}{source.created && <> · <time dateTime={source.created}>{new Date(source.created).toLocaleString()}</time></>}</li>;
    })}</ul>
  </details>;
}

export function MemoryKnowledge({ path, questions, archived, request }: {
  path: string; questions: boolean; archived: boolean; request: <T>(path: string, data?: unknown) => Promise<T>;
}) {
  const endpoint = `${path}/${questions ? 'questions' : 'knowledge'}`;
  const [query, setQuery] = useState(''), [search, setSearch] = useState(''), [kind, setKind] = useState('all');
  const [cursors, setCursors] = useState<number[]>([]), [refresh, setRefresh] = useState(0);
  const [loadError, setLoadError] = useState(''), [saveError, setSaveError] = useState('');
  const [edit, setEdit] = useState<Editor | null>(null);
  const [notice, setNotice] = useState<{ text: string; undo?: Change } | null>(null);
  const [pending, setPending] = useState<Change | null>(null);
  const [busy, setBusy] = useState(false);
  const sending = useRef(false), searchInput = useRef<HTMLInputElement>(null), addButton = useRef<HTMLButtonElement>(null);
  const pagePath = questions ? endpoint : `${endpoint}?${new URLSearchParams({ query: search, kind, after: String(cursors.at(-1) ?? 0) })}`;
  const { data: page, fresh } = useCachedData<RecordPage>(pagePath);
  const root = useVisiblePolling(useCallback(async current => {
    try {
      await queryCache.refresh(pagePath, () => request<RecordPage>(pagePath));
      if (current()) setLoadError('');
    } catch (cause) {
      if (current()) setLoadError(cause instanceof Error ? cause.message : 'Memory could not load.');
    }
  }, [pagePath, refresh, request]));

  async function send(change: Change) {
    if (sending.current || archived) return;
    sending.current = true;
    setBusy(true); setPending(change); setSaveError('');
    try {
      const { revision } = await request<{ revision: number }>(endpoint, change);
      setPending(null); setEdit(null); setRefresh(n => n + 1);
      if (change.action === 'forget' || change.action === 'forget_question') {
        const id = crypto.randomUUID();
        const undo: Change = 'unit' in change
          ? { id, action: 'restore', unit: change.unit, revision }
          : { id, action: 'restore_question', model: change.model, revision };
        setNotice({ text: questions ? 'Question forgotten.' : 'Memory forgotten.', undo });
        (questions ? addButton.current : searchInput.current)?.focus();
      } else {
        setNotice({ text: change.action.startsWith('restore') ? 'Restored.' : 'Saved.' });
      }
    } catch (cause) {
      const rejected = cause && typeof cause === 'object' && 'status' in cause && Number(cause.status) < 500;
      if (rejected) { setPending(null); setRefresh(n => n + 1); }
      setSaveError(cause instanceof Error ? cause.message : 'Could not confirm change.');
    } finally { sending.current = false; setBusy(false); }
  }

  const locked = archived || busy || !!pending || !fresh;
  function open(item?: Knowledge | Question) {
    setSaveError('');
    setEdit({ item, text: item && ('question' in item ? item.question : item.text) || '', model: typeof item?.id === 'string' ? item.id : crypto.randomUUID() });
  }

  const visible = page?.items.filter(item => !questions || 'question' in item && item.question.toLowerCase().includes(search.toLowerCase()));
  const error = saveError && <Alert tone="error" actions={pending && <Button size="sm" disabled={busy} onClick={() => void send(pending)}>Retry change</Button>}>{saveError}</Alert>;
  return <div ref={root} className="k-stack memory-view">
    {(notice || saveError) && <div className="k-stack">
      {!edit && error}
      {notice && <Alert actions={notice.undo && <Button size="sm" disabled={archived || busy || !!pending} onClick={() => { if (notice.undo) void send(notice.undo); }}>Undo</Button>}>{notice.text}</Alert>}
    </div>}
    <form className="memory-toolbar" role="search" onSubmit={event => { event.preventDefault(); setSearch(query.trim()); setCursors([]); setRefresh(n => n + 1); }}>
      <Field label={questions ? 'Search questions' : 'Search learned memory'}><Input ref={searchInput} type="search" maxLength={200} value={query} onChange={event => setQuery(event.target.value)} /></Field>
      {!questions && <Field label="Type"><Select value={kind} onChange={value => { setKind(value); setCursors([]); }} options={[
        { value: 'all', label: 'All' }, { value: 'facts', label: 'Facts' }, { value: 'observations', label: 'Observations' },
      ]} /></Field>}
      <Button type="submit">Search</Button>
      {questions && <Button ref={addButton} variant="primary" disabled={locked} onClick={() => open()}>Add question</Button>}
    </form>
    {loadError && <Alert tone="error" actions={<Button size="sm" onClick={() => setRefresh(n => n + 1)}>Retry</Button>}>{loadError}</Alert>}
    {!page && !loadError && <><span role="status" className="sr-only">Loading memory</span><SkeletonRows /></>}
    {visible && (visible.length ? <Card><ul className="memory-list" role="list">{visible.map(item => {
      const question = 'question' in item;
      const title = question ? item.question : item.text;
      const sources = question ? item.reflection?.evidence.flatMap(evidence => evidence.source) ?? [] : item.sources;
      const revision = item.revision;
      const id = item.id;
      return <li key={id}>
        <Row title={<span className="memory-text">{title}</span>}
          meta={question ? item.pending ? 'Awaiting refresh' : undefined : <>{item.kind === 'observation' ? 'Observation' : item.kind === 'experience' ? 'Experience' : 'Fact'}{item.essential && <> · <Badge>Pinned</Badge></>}</>}
          trailing={<ActionMenu label={question ? 'Question actions' : 'Memory actions'}
            trigger={<button type="button" className="k-icon-btn" aria-label={question ? 'Question actions' : 'Memory actions'} disabled={locked}><Ellipsis size={16} aria-hidden /></button>}
            items={[
              { label: 'Edit', onSelect: () => open(item) },
              ...(!question ? [{ label: item.essential ? 'Unpin' : 'Pin to context', onSelect: () => void send({ id: crypto.randomUUID(), action: 'pin', unit: item.id, revision, essential: !item.essential }) }] : []),
              { label: 'Forget', danger: true, onSelect: () => void send(question ? { id: crypto.randomUUID(), action: 'forget_question', model: item.id, revision } : { id: crypto.randomUUID(), action: 'forget', unit: item.id, revision }) },
            ]} />}>
          {question && item.reflection && <span className="memory-answer">{item.reflection.answer}</span>}
        </Row>
        <Sources sources={sources} />
      </li>;
    })}</ul></Card> : <EmptyState title={search ? 'No matches' : questions ? 'No standing questions yet' : 'No learned memory yet'} />)}
    {(cursors.length > 0 || page?.next_after) && <div className="k-pager">
      <IconButton label="Previous page" disabled={cursors.length === 0} onClick={() => setCursors(items => items.slice(0, -1))}><ChevronLeft size={16} aria-hidden /></IconButton>
      <IconButton label="Next page" disabled={!page?.next_after || !fresh} onClick={() => { if (page?.next_after) setCursors(items => [...items, page.next_after!]); }}><ChevronRight size={16} aria-hidden /></IconButton>
    </div>}
    {edit && <Dialog open title={questions ? edit.item ? 'Edit question' : 'Add standing question' : 'Edit memory'} onOpenChange={next => { if (!next && !busy && !pending) setEdit(null); }}>
      <form className="k-stack" onSubmit={event => {
        event.preventDefault();
        const id = crypto.randomUUID();
        if (questions) void send({ id, action: 'save_question', model: edit.model, revision: edit.item?.revision ?? 0, question: edit.text });
        else if (typeof edit.item?.id === 'number') void send({ id, action: 'edit', unit: edit.item.id, revision: edit.item.revision, text: edit.text });
      }}>
        <Field label={questions ? 'Question' : 'Memory contents'}><Textarea required autoFocus maxLength={2000} value={edit.text} readOnly={busy || !!pending} onChange={event => setEdit({ ...edit, text: event.target.value })} /></Field>
        {error}
        <div className="k-row-actions">
          <Button type="submit" variant="primary" busy={busy} disabled={archived || !!pending || !edit.text.trim()}>Save</Button>
          <Button type="button" disabled={busy || !!pending} onClick={() => setEdit(null)}>Cancel</Button>
        </div>
      </form>
    </Dialog>}
  </div>;
}

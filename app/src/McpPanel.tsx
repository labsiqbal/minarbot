import { useCachedData, useQueryPolling } from './hooks/useCachedQuery';
import { queryCache } from './hooks/queryCache';
import { getNonce } from 'get-nonce';
import { useEffect, useRef, useState } from 'react';
import { ArrowLeft, ChevronLeft, ChevronRight, Info, Ellipsis, Plug, Plus } from 'lucide-react';
import { ActionMenu, Alert, Badge, Button, Card, Checkbox, CodeBlock, Collapsible, Dialog, EmptyState, Field, IconButton, Input, PageHeader, Row, Section, Select, SkeletonRows, Tooltip, useConfirm, RecoveryAlert } from './ui/kit';
type Connection = { id: string; name: string; url: string; hasToken: boolean; enabled: number; revision: number };
type Tool = { name: string; title?: string; description?: string; inputSchema: unknown; _meta?: Record<string, unknown> };
const kind = (tool: Tool) => tool._meta?.['minarbot.kind'];
const label = (tool: Tool) => `${kind(tool) === 'resource-template' ? 'Resource template: ' : kind(tool) === 'resource' ? 'Resource: ' : kind(tool) === 'prompt' ? 'Prompt: ' : 'Tool: '}${tool.title && tool.title !== tool.name && !kind(tool) ? `${tool.title} (${tool.name})` : tool.title ?? tool.name}`;
type Detail = Connection & { tools: Tool[]; grant: { revision: number; tools: string[] } };
type Command = { path: string; body: Record<string, unknown> & { id: string } };
function restore(raw: string | null): Command | null {
  if (raw === null) return null;
  if (raw.length > 16000) throw new Error('MCP recovery data is too large.');
  const value = JSON.parse(raw), uuid = '[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}';
  if (!value || typeof value.path !== 'string' || !new RegExp(`^/api/(mcp(?:/${uuid}(?:/remove)?)?|agents/${uuid}/mcp/${uuid})$`, 'i').test(value.path) || !value.body || typeof value.body !== 'object' || !new RegExp(`^${uuid}$`, 'i').test(value.body.id) || Object.keys(value.body).some(key => !['id', 'test', 'revision', 'enabled', 'connectionRevision', 'tools', 'confirmed'].includes(key))) throw new Error('MCP recovery data is invalid.');
  return value;
}
export function McpPanel({ agents, request, project }: { project?: string; agents: { id: string; name: string }[]; request: <T>(path: string, body?: unknown) => Promise<T>; onClose: () => void }) {
  const storage = 'minarbot:mcp-command'+(project?`:${project}`:'');
  const [recovery] = useState(() => { try { return { command: restore(sessionStorage.getItem(storage)), invalid: false }; } catch { return { command: null, invalid: true }; } });
  const [pending, setPending] = useState(recovery.command), [invalid, setInvalid] = useState(recovery.invalid);
  const [cursors, setCursors] = useState(['']);
  const [agent, setAgent] = useState(agents[0]?.id ?? ''), [selected, setSelected] = useState(''), [detail, setDetail] = useState<Detail | null>(null), [tools, setTools] = useState<string[]>([]);
  const [name, setName] = useState(''), [url, setUrl] = useState(''), [token, setToken] = useState('');
  const [editing, setEditing] = useState<Connection | null>(null), [rotate, setRotate] = useState(false);
  const [tested, setTested] = useState<{ id: string; tools: Tool[]; expires: number } | null>(null);
  const [busy, setBusy] = useState(false), [error, setError] = useState(''), [refresh, setRefresh] = useState(0);
  const generation = useRef(0), reads = useRef(0), flight = useRef(false), heading = useRef<HTMLHeadingElement>(null);
  const [showForm, setShowForm] = useState(false);
  const confirm = useConfirm();
  const locked = busy || !!pending || invalid, after = cursors.at(-1)!;
  useEffect(() => { heading.current?.focus(); return () => { generation.current++; }; }, []);
  useEffect(() => { heading.current?.focus(); }, [after, selected]);
  const path = `/api/mcp${after ? `?after=${after}` : ''}`;
  const cachePath = project ? `/api/projects/${project}/${path.slice(5)}` : path;
  const { data: page, fresh } = useCachedData<{ items: Connection[]; next_after: string | null }>(cachePath);
  useQueryPolling(async signal => {
    const current = ++reads.current;
    try {
      const [, nextDetail] = await Promise.all([queryCache.refresh(cachePath, () => request<NonNullable<typeof page>>(path)), selected && agent ? request<Detail>(`/api/mcp/${selected}?agent=${agent}`) : Promise.resolve(null)]);
      if (!signal.aborted && current === reads.current) {
        setDetail(nextDetail); setError('');
        setTools(nextDetail?.grant.tools ?? []);
      }
    } catch (error) { if (!signal.aborted && current === reads.current) setError(error instanceof Error ? error.message : 'Connections could not load.'); }
  }, { interval: 5000, enabled: !busy && !showForm && (!detail || JSON.stringify(tools) === JSON.stringify(detail.grant.tools)) }, [after, selected, agent, refresh, request]);
  useEffect(() => {
    if (!tested) return;
    const timer = window.setTimeout(() => setTested(null), Math.max(0, tested.expires - Date.now()));
    return () => window.clearTimeout(timer);
  }, [tested]);
  async function reload() {
    if (busy || flight.current) return;
    if (!editing || pending || invalid) { setRefresh(value => value + 1); return; }
    if (!await confirm({ title: 'Load the latest connection revision for this draft?', description: 'The entered name and token stay saved, but old test results are discarded. Test again before saving.', confirm: 'Reload' })) return;
    const current = generation.current; flight.current = true; setBusy(true); setError(''); setTested(null);
    try { const latest = await request<Detail>(`/api/mcp/${editing.id}`); if (current === generation.current) { setEditing(latest); setRefresh(value => value + 1); } }
    catch (error) { if (current === generation.current) setError(error instanceof Error ? error.message : 'Connection could not reload.'); }
    finally { flight.current = false; if (current === generation.current) setBusy(false); }
  }
  async function test() {
    if (flight.current || locked) return;
    const current = generation.current; flight.current = true; setBusy(true); setError(''); setTested(null);
    try { const result = await request<NonNullable<typeof tested>>('/api/mcp/test', { id: crypto.randomUUID(), name, ...(editing ? { connection: editing.id, revision: editing.revision, ...(rotate ? { token } : {}) } : { url, token }) }); if (current === generation.current) setTested(result); }
    catch (error) { if (current === generation.current) setError(error instanceof Error ? error.message : 'Connection test failed.'); }
    finally { flight.current = false; if (current === generation.current) setBusy(false); }
  }
  async function execute(command: Command) {
    if (flight.current || busy || invalid) return;
    const current = generation.current;
    try {
      restore(JSON.stringify(command)); sessionStorage.setItem(storage, JSON.stringify(command)); setPending(command); setBusy(true); flight.current = true; reads.current++; setError('');
      await request(command.path, command.body);
      if (current !== generation.current) return;
      if (sessionStorage.getItem(storage) === JSON.stringify(command)) sessionStorage.removeItem(storage);
      setPending(null); setDetail(null);
      if (command.body.test && (editing ? command.path === `/api/mcp/${editing.id}` : command.path === '/api/mcp')) { setTested(null); setEditing(null); setRotate(false); setToken(''); setName(''); setUrl(''); setShowForm(false); }
      else if (editing && command.path === `/api/mcp/${editing.id}`) setTested(null);
      if(command.path.endsWith('/remove')){setSelected('');setDetail(null);}
      setRefresh(value => value + 1); heading.current?.focus();
    } catch (error) { if (current === generation.current) setError(error instanceof Error ? error.message : 'Changes are not confirmed yet. Try again.'); }
    finally { flight.current = false; if (current === generation.current) setBusy(false); }
  }
  const ignoreRecovery = <Button size="sm" disabled={busy} onClick={async () => { if (!await confirm({ title: 'Discard saved request?', description: 'Changes may already have succeeded. Check the list after it reloads.', confirm: 'Discard request' })) return; try { sessionStorage.removeItem(storage); setPending(null); setInvalid(false); setRefresh(value => value + 1); } catch { setError('Browser storage could not be updated.'); } }}>Discard request</Button>;
  const recoveryNotice = <>
    {invalid && <Alert tone="error" title="Recovery unavailable" actions={!pending && ignoreRecovery} />}
    {pending && <RecoveryAlert inFlight={busy} tone="warn" title="Not confirmed" actions={<><Button size="sm" variant="primary" disabled={busy} onClick={() => void execute(pending)}>Try again</Button>{ignoreRecovery}</>} />}
  </>;
  const host = (value: string) => { try { return new URL(value).host; } catch { return value; } };
  const connection = detail ?? page?.items.find(row => row.id === selected);
  const edit = async (row: Connection) => {
    if ((name || url || token) && !await confirm({ title: 'Discard the current connection draft and open this connection?', confirm: 'Discard draft', danger: true })) return;
    generation.current++; setEditing(row); setName(row.name); setUrl(row.url); setToken(''); setRotate(false); setTested(null); heading.current?.focus(); setShowForm(true);
  };
  const open = (row: Connection) => { generation.current++; reads.current++; setDetail(null); setSelected(row.id); setRefresh(value => value + 1); };
  const menu = (row: Connection) => <ActionMenu label={`Menu ${row.name}`} trigger={<button type="button" className="k-icon-btn" aria-label={`Menu ${row.name}`} disabled={locked || !fresh}><Ellipsis size={16} aria-hidden /></button>} items={[
    { label: 'Edit connection', onSelect: () => void edit(row) },
    { label: `${row.enabled ? 'Disable' : 'Enable'} ${row.name}`, onSelect: () => void execute({ path: `/api/mcp/${row.id}`, body: { id: crypto.randomUUID(), revision: row.revision, enabled: !row.enabled } }) },
    'separator',
    { label: `Remove ${row.name}`, danger: true, onSelect: async () => { if(await confirm({title:`Remove ${row.name}?`,description:'This removes the saved connection and revokes agent access. External effects already completed remain.',confirm:'Remove connection',danger:true}))void execute({path:`/api/mcp/${row.id}/remove`,body:{id:crypto.randomUUID(),confirmed:true}}); } },
  ]} />;
  return <div className="k-page bs-root bs-set3-mcp">
    <style nonce={getNonce()}>{`.bs-root.bs-set3-mcp .k-row__title { white-space: normal; overflow-wrap: anywhere; } .bs-root.bs-set3-mcp .k-row__meta { overflow-wrap: anywhere; }`}</style>
    {selected && <Button aria-label="Back to MCP connections" variant="ghost" icon={<ArrowLeft size={14} aria-hidden />} onClick={() => { setSelected(''); setDetail(null); }}>MCP connections</Button>}
    <PageHeader headingRef={heading} title={selected ? connection?.name ?? 'Connection' : 'MCP connections'}
      actions={<>{selected && connection ? <>{editing && <Button disabled={locked} onClick={() => setShowForm(true)}>Continue editing</Button>}{menu(connection)}</> : <Button variant="primary" icon={<Plus size={14} aria-hidden />} disabled={locked} onClick={() => setShowForm(true)}>{editing ? 'Continue editing' : 'Add connection'}</Button>}</>} />
    {error && <Alert tone="error">{error}</Alert>}{recoveryNotice}
    {!selected ? <div id="mcp-connections" className="k-stack">
      {!page ? !error && <><span className="sr-only" role="status">Loading connections...</span><SkeletonRows /></> : page.items.length ? <Card>{page.items.map(row => <Row key={row.id} title={row.name} leading={<Plug size={16} aria-hidden />} disabled={locked} onClick={() => open(row)}
        meta={<><span>{host(row.url)}</span><Badge tone={row.enabled ? 'success' : 'neutral'}>{row.enabled ? 'Active' : 'Inactive'}</Badge></>}
        trailing={menu(row)} />)}</Card> : <EmptyState icon={<Plug size={18} />} title="No connections" />}
      {(cursors.length > 1 || page?.next_after) && <div className="k-pager">
        <IconButton label="Previous connections" disabled={locked || cursors.length < 2} onClick={() => { generation.current++; setCursors(value => value.slice(0, -1)); }}><ChevronLeft size={16} aria-hidden /></IconButton>
        <IconButton label="Next connections" disabled={locked || !page?.next_after} onClick={() => { generation.current++; setCursors(value => [...value, page!.next_after!]); }}><ChevronRight size={16} aria-hidden /></IconButton>
      </div>}
    </div> : <>
      {connection && <Card><Row title="Status" trailing={<Badge tone={connection.enabled ? 'success' : 'neutral'}>{connection.enabled ? 'Active' : 'Inactive'}</Badge>} /><Row title="URL" meta={connection.url} /><Row title="Token" trailing={<Badge>{connection.hasToken ? 'Saved' : 'None'}</Badge>} /></Card>}
      <Section id="mcp-agent-access" title="Permissions" actions={<Select size="sm" label="Agent" value={agent} disabled={locked} onChange={value => { generation.current++; reads.current++; setDetail(null); setAgent(value); }} options={agents.map(item => ({ value: item.id, label: item.name.length > 24 ? item.name.slice(0, 21) + '...' : item.name }))} />}>
        {detail ? <form className="k-stack" onSubmit={async event => { event.preventDefault(); if (!locked && await confirm({ title: `Allow selected access for ${agents.find(item => item.id === agent)?.name}?`, description: 'Effects in external services may still occur after the task is canceled.', confirm: 'Save permissions' })) void execute({ path: `/api/agents/${agent}/mcp/${detail.id}`, body: { id: crypto.randomUUID(), revision: detail.grant.revision, connectionRevision: detail.revision, tools } }); }}>
          <fieldset disabled={locked} className="k-fieldset">
            {detail.tools.length ? <Card>{detail.tools.map(tool => <div key={tool.name}>
              <Row title={<Checkbox label={label(tool)} checked={tools.includes(tool.name)} disabled={locked} onChange={checked => setTools(value => checked ? [...value, tool.name] : value.filter(name => name !== tool.name))} />} />
              <div style={{ padding: '0 16px 12px' }}><Collapsible title={`Details: ${tool.title ?? tool.name}`}>
                {tool.description && <p className="k-field__hint">{tool.description}</p>}
                {kind(tool) === 'resource-template' && <Alert tone="warn">Permissions cover all URIs this template can form, including parameter values chosen by the agent. The target server still controls access to its data.</Alert>}
                {typeof tool._meta?.['minarbot.target'] === 'string' && <span className="k-field__hint" style={{ overflowWrap: 'anywhere' }}>{tool._meta['minarbot.target']}</span>}
                <CodeBlock label={`Parameters for ${label(tool)}`}>{JSON.stringify(tool.inputSchema, null, 2)}</CodeBlock>
              </Collapsible></div>
            </div>)}</Card> : <EmptyState title="No tools, resources or prompts" />}
          </fieldset>
          <div className="k-row-actions"><Button type="submit" variant="primary" disabled={locked}>Save permissions</Button></div>
        </form> : !agent ? <EmptyState icon={<Plug size={18} />} title="No agents yet" /> : !error && <><span className="sr-only" role="status">Loading permissions...</span><SkeletonRows /></>}
      </Section>
    </>}
    <Dialog open={showForm} size="lg" onOpenChange={setShowForm} title={editing ? `Edit ${editing.name}` : 'New connection'}
      footer={<>
        {editing && <Button variant="ghost" disabled={busy} onClick={() => void reload()}>Load current revision</Button>}
        {editing && <Button className="k-dialog__foot-start" disabled={locked} onClick={() => { setEditing(null); setRotate(false); setName(''); setUrl(''); setToken(''); setTested(null); }}>Cancel editing</Button>}
        <Button variant={tested ? 'secondary' : 'primary'} type="submit" form="mcp-connection" disabled={locked} busy={busy}>Test connection</Button>
        {tested && <Button variant="primary" disabled={locked || tested.expires <= Date.now()} onClick={async () => { if (editing && !await confirm({ title: 'Save the tested catalog and token, revoke all access permissions, and stop active MCP calls for this connection?', description: 'External effects that have already occurred cannot be undone.', confirm: 'Save changes', danger: true })) return; void execute({ path: editing ? `/api/mcp/${editing.id}` : '/api/mcp', body: { id: crypto.randomUUID(), test: tested.id, ...(editing ? { revision: editing.revision } : {}) } }); }}>{editing ? 'Save changes' : 'Save connection'}</Button>}
      </>}>
      {error && <Alert tone="error">{error}</Alert>}
      <form id="mcp-connection" onSubmit={event => { event.preventDefault(); void test(); }}>
        {recoveryNotice}
        <fieldset disabled={locked} className="k-fieldset">
          <Field label="Name"><Input required maxLength={80} value={name} onChange={event => { setName(event.target.value); setTested(null); }} /></Field>
          <Field label="URL"><Input required readOnly={!!editing} type="url" maxLength={2000} value={url} onChange={event => { setUrl(event.target.value); setTested(null); }} /></Field>
          {editing && <Checkbox checked={rotate} label="Replace token" onChange={checked => { setRotate(checked); setToken(''); setTested(null); }} />}
          {(!editing || rotate) && <Field label="Token" optional><Input type="password" autoComplete="off" maxLength={4096} value={token} onChange={event => { setToken(event.target.value); setTested(null); }} /></Field>}
          <Tooltip content="Streamable HTTP. Testing contacts the service. Saved URLs cannot be changed. Token uses Bearer authentication; an empty replacement removes authentication."><IconButton label="Connection details"><Info size={14} aria-hidden /></IconButton></Tooltip>
          {tested && <>
            <div className="k-inline"><Tooltip content="Valid for 10 minutes"><span tabIndex={0}><Badge tone="success">Tested</Badge></span></Tooltip><span className="k-field__hint">{tested.tools.length} items</span></div>
            <Card>{tested.tools.map(tool => <Row key={tool.name} title={label(tool)} />)}</Card>
          </>}
        </fieldset>
      </form>
    </Dialog>
  </div>;
}

import { copyText } from './ui/clipboard';
import { getNonce } from 'get-nonce';
import { useCachedData, useQueryPolling } from './hooks/useCachedQuery';
import { queryCache } from './hooks/queryCache';
import { useEffect, useRef, useState } from 'react';
import { Copy, Ellipsis, KeyRound, Plus } from 'lucide-react';
import { ActionMenu, Alert, Badge, Button, Card, Checkbox, Dialog, EmptyState, Field, IconButton, Input, PageHeader, Row, Section, SkeletonRows, Tooltip, useConfirm, RecoveryAlert } from './ui/kit';
import { mcpScopeCatalog as scopes, type McpScope as Scope } from '../shared/mcp-scopes';
type PeerToken = { id: string; name: string; scopes: Scope[]; created: number; lastUsed: number | null; revoked: number | null };
type Command = { path: string; body: { id: string; name?: string; scopes?: Scope[] } };
const scopeLabels: Partial<Record<Scope, string>> = {
  'history:read': 'Read token conversations',
  orchestration: 'Delegate and use app tools',
  'machine:update': 'Update paired machines',
};
const scopeLabel = (id: Scope) => scopeLabels[id] ?? scopes.find(scope => scope.id === id)?.label ?? id;
const path = '/api/mcp-server/peer-tokens', storage = 'minarbot:mcp-server-command';
function restore(raw: string | null): Command | null {
  if (!raw) return null;
  const value = JSON.parse(raw), uuid = '[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}';
  if (raw.length > 2000 || !value || !new RegExp(`^${path}(?:/${uuid}/revoke)?$`, 'i').test(value.path) || !value.body || !new RegExp(`^${uuid}$`, 'i').test(value.body.id) || Object.keys(value.body).some(key => !['id','name','scopes'].includes(key)) || (value.path === path && (typeof value.body.name !== 'string' || value.body.name.length > 80 || !Array.isArray(value.body.scopes) || !value.body.scopes.length || value.body.scopes.some((scope: unknown) => !scopes.some(item => item.id === scope))))) throw new Error('Invalid MCP server recovery data.');
  return value;
}
export function McpServerPanel({ request }: { request: <T>(path: string, body?: unknown) => Promise<T> }) {
  const [recovery] = useState(() => { try { return { pending: restore(sessionStorage.getItem(storage)), invalid: false }; } catch { return { pending: null, invalid: true }; } });
  const [pending, setPending] = useState(recovery.pending), [invalid, setInvalid] = useState(recovery.invalid);
  const { data: page } = useCachedData<{ items: PeerToken[]; endpoint: string }>(path);
  const [refresh, setRefresh] = useState(0);
  const [detailId, setDetailId] = useState('');
  const [showForm, setShowForm] = useState(false), [name, setName] = useState(''), [selected, setSelected] = useState<Scope[]>(['agents:read']);
  const [secret, setSecret] = useState(''), [error, setError] = useState(''), [notice, setNotice] = useState(''), [busy, setBusy] = useState(false);
  const heading = useRef<HTMLHeadingElement>(null), mounted = useRef(true), flight = useRef(false), reads = useRef(0);
  const confirm = useConfirm(), locked = busy || !!pending || invalid;
  useEffect(() => { mounted.current = true; heading.current?.focus(); return () => { mounted.current = false; reads.current++; }; }, []);
  useQueryPolling(async signal => {
    const version = ++reads.current;
    try { await queryCache.refresh(path, () => request<NonNullable<typeof page>>(path)); if (!signal.aborted && mounted.current && version === reads.current) setError(''); }
    catch (reason) { if (!signal.aborted && mounted.current && version === reads.current) setError(reason instanceof Error ? reason.message : 'Access tokens could not load.'); }
  }, { interval: 5000, enabled: !busy && !pending && !invalid }, [refresh, request]);
  async function copy(value: string) { try { await copyText(value); if (mounted.current) { setError(''); setNotice('Copied.'); } } catch { if (mounted.current) { setNotice(''); setError('Could not copy. Select the text and copy it manually.'); } } }
  async function execute(command: Command) {
    if (flight.current || busy || invalid) return;
    flight.current = true; setBusy(true); setError(''); setNotice(''); setSecret('');
    try {
      restore(JSON.stringify(command)); sessionStorage.setItem(storage, JSON.stringify(command)); setPending(command);
      const result = await request<PeerToken & { token?: string; alreadyCreated?: boolean }>(command.path, command.body);
      if (!mounted.current) return;
      sessionStorage.removeItem(storage); setPending(null); setShowForm(false); setName(''); setSelected(['agents:read']);
      setSecret(result.token ?? '');
      if (result.alreadyCreated) setNotice('Token was already created. Its secret cannot be recovered. Revoke it and create a replacement if you did not save it.');
      setRefresh(value => value + 1); heading.current?.focus();
    } catch (error) { if (mounted.current) setError(error instanceof Error ? error.message : 'Change is not confirmed. Retry the saved request.'); }
    finally { flight.current = false; if (mounted.current) setBusy(false); }
  }
  async function discard() {
    if (!await confirm({ title: 'Discard the saved request?', description: 'The change may already have succeeded. Check the token list before creating a replacement.', confirm: 'Discard request', danger: true })) return;
    try { sessionStorage.removeItem(storage); setPending(null); setInvalid(false); setRefresh(value => value + 1); } catch { setError('Browser storage could not be updated.'); }
  }
  const recoveryNotice = <>
    {invalid && <Alert tone="error" title="Recovery unavailable" actions={<Button size="sm" onClick={() => void discard()}>Discard request</Button>} />}
    {pending && <RecoveryAlert inFlight={busy} tone="warn" title="Not confirmed" actions={<><Button size="sm" disabled={busy} onClick={() => void execute(pending)}>Try again</Button><Button size="sm" disabled={busy} onClick={() => void discard()}>Discard request</Button></>}>A created token is never revealed twice.</RecoveryAlert>}
  </>;
  const detail = page?.items.find(token => token.id === detailId);
  return <div className="k-page bs-root bs-set3-external">
    <style nonce={getNonce()}>{`.bs-root.bs-set3-external .k-row__title { white-space: normal; overflow-wrap: anywhere; } .bs-root.bs-set3-external .k-row__meta { overflow-wrap: anywhere; }`}</style>
    <PageHeader headingRef={heading} title="External access"
      actions={<><Button variant="primary" icon={<Plus size={14} aria-hidden />} disabled={locked} onClick={() => setShowForm(true)}>Create token</Button></>} />
    {error && <Alert tone="error" actions={<Button size="sm" onClick={() => setRefresh(value => value + 1)}>Retry</Button>}>{error}</Alert>}
    {notice && (notice === 'Copied.' ? <span role="status" className="sr-only">{notice}</span> : <Alert>{notice}</Alert>)}
    {recoveryNotice}
    <Field label={<Tooltip content="Remote clients need an allowed address."><span tabIndex={0}>Endpoint</span></Tooltip>}>
      <div className="k-inline"><Input readOnly value={page ? new URL('/mcp', window.location.href).href : ''} onFocus={event => event.target.select()} /><IconButton label="Copy endpoint URL" disabled={!page} onClick={() => void copy(new URL('/mcp', window.location.href).href)}><Copy size={15} aria-hidden /></IconButton></div>
    </Field>
    <Section title="Access tokens">
      {!page ? !error && <><span role="status" className="sr-only">Loading access tokens...</span><SkeletonRows /></> : !page.items.length ? <EmptyState icon={<KeyRound size={18} />} title="No access tokens" /> : <Card>{page.items.map(token => <Row key={token.id} title={token.name} leading={<KeyRound size={16} aria-hidden />} onClick={() => setDetailId(token.id)}
        meta={<><Badge tone={token.revoked === null ? 'success' : 'neutral'}>{token.revoked === null ? 'Active' : 'Revoked'}</Badge><span>{token.scopes.length} permissions</span><span>{token.lastUsed === null ? 'Never used' : `Last used ${new Date(token.lastUsed).toLocaleString('en-US')}`}</span></>}
        trailing={token.revoked === null && <ActionMenu label={`Token actions for ${token.name}`} trigger={<IconButton label={`Token actions for ${token.name}`} tooltip="More actions" disabled={locked}><Ellipsis size={16} aria-hidden /></IconButton>} items={[{ label: 'Revoke token', danger: true, onSelect: async () => { if (await confirm({ title: `Revoke ${token.name}?`, description: 'This token will stop working immediately. Work already queued or running will continue.', confirm: 'Revoke token', danger: true })) void execute({ path: `${path}/${token.id}/revoke`, body: { id: crypto.randomUUID() } }); } }]} />} />)}</Card>}
    </Section>
    <Dialog open={!!detail} onOpenChange={open => { if (!open) setDetailId(''); }} title={detail?.name ?? 'Token permissions'}>
      {detail && <div className="k-stack"><div><Badge tone={detail.revoked === null ? 'success' : 'neutral'}>{detail.revoked === null ? 'Active' : 'Revoked'}</Badge></div><Section title="Permissions"><Card>{detail.scopes.map(id => <Row key={id} title={scopeLabel(id)} />)}</Card></Section></div>}
    </Dialog>
    <Dialog open={!!secret} onOpenChange={() => {}} title="Copy token now" hideHeader footer={<Button variant="primary" onClick={() => { setSecret(''); setNotice(''); }}>I saved the token</Button>}>
      <div className="k-stack">
        <h2 tabIndex={0} className="k-dialog__title">Copy token now</h2>
        <div className="k-inline"><Tooltip content="Grants selected permissions. Store securely."><span tabIndex={0}><Badge tone="warn">Shown once</Badge></span></Tooltip></div>
        {error && <Alert tone="error">{error}</Alert>}
        {notice === 'Copied.' && <span role="status">{notice}</span>}
        <Field label="Token"><div className="k-inline"><Input data-private-token readOnly value={secret} autoComplete="off" spellCheck={false} onFocus={event => event.target.select()} /><IconButton label="Copy token" onClick={() => void copy(secret)}><Copy size={15} aria-hidden /></IconButton></div></Field>
      </div>
    </Dialog>
    <Dialog open={showForm} size="lg" onOpenChange={setShowForm} title="Create token" footer={<Button variant="primary" type="submit" form="create-peer-token" busy={busy} disabled={locked || !name.trim() || !selected.length}>Create token</Button>}>
      {error && <Alert tone="error">{error}</Alert>}{recoveryNotice}
      <form id="create-peer-token" onSubmit={event => { event.preventDefault(); if (!locked) void execute({ path, body: { id: crypto.randomUUID(), name, scopes: selected } }); }}>
        <fieldset disabled={locked} className="k-fieldset">
          <Field label={<Tooltip content="Shown as the sender of messages from this token."><span tabIndex={0}>Name</span></Tooltip>}><Input required maxLength={80} value={name} onChange={event => setName(event.target.value)} placeholder="Work laptop" /></Field>
          <div role="group" aria-label="Permissions">{(['Read', 'Write', 'Run'] as const).map(group => <fieldset className="mcp-scope-group" key={group}><legend>{group}</legend><div className="k-grid-2">{scopes.filter(scope => scope.group === group).map(scope => <div className="k-inline" key={scope.id}>
            <Tooltip content={scope.hint}><span tabIndex={0}><Checkbox label={scopeLabel(scope.id)} checked={selected.includes(scope.id)} onChange={checked => setSelected(value => checked ? [...value, scope.id] : value.filter(item => item !== scope.id))} /></span></Tooltip>
          </div>)}</div></fieldset>)}</div>
        </fieldset>
      </form>
    </Dialog>
  </div>;
}

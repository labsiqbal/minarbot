import { usePolling } from './hooks/usePolling';
import { WhatsAppSettings } from './WhatsAppSettings';
import { getNonce } from 'get-nonce';
import { useEffect, useRef, useState } from 'react';
import { ArrowLeft, Blocks, ChevronLeft, ChevronRight, Ellipsis, Search } from 'lucide-react';
import { ActionMenu, Alert, Badge, Button, Card, Dialog, EmptyState, Field, IconButton, Input, PageHeader, Row, Section, Select, SkeletonRows, Switch, Tooltip, useConfirm, RecoveryAlert } from './ui/kit';
import type { LocalProject } from '../shared/local-project';
import { restoreConnectedAppCommand, type ConnectedAppCommand } from './connected-app-command';
type Toolkit = { slug: string; name: string; description: string; managedAuth: boolean };
type Catalog = { configured: boolean; items: Toolkit[]; categories: { id: string; name: string }[]; next_after: string | null };
type KeyStatus = { source: 'settings' | 'environment' | 'none'; revision: number; last4?: string };
type Account = { id: string; toolkit: string; status: string };
type Grant = { toolkit: string; enabled: boolean; revision: number };
type Policy = { toolkit: string; tool: string; account: string; fingerprint: string };
export function ConnectedAppsPanel({ agents, sections = [], request, project, onInstallationAppsChange }: { project?: LocalProject; onInstallationAppsChange?: (enabled: boolean) => Promise<void>; agents: { id: string; name: string }[]; sections?: { id: string; name: string }[]; request: <T>(path: string, data?: unknown) => Promise<T> }) {
  const storage = 'minarbot:connected-app-command'+(project?`:${project.id}`:'');
  const confirm = useConfirm(), heading = useRef<HTMLHeadingElement>(null), mounted = useRef(false), sending = useRef(false);
  const [recovery] = useState(() => { try { return { pending: restoreConnectedAppCommand(sessionStorage.getItem(storage)), invalid: false }; } catch { return { pending: null, invalid: true }; } });
  const [pending,setPending] = useState(recovery.pending), [invalid,setInvalid] = useState(recovery.invalid);
  const [catalog,setCatalog] = useState<Catalog | null>(null), [accounts,setAccounts] = useState<Account[] | null>(null), [grants,setGrants] = useState<Grant[] | null>(null), [policies,setPolicies] = useState<Policy[] | null>(null), [keyStatus,setKeyStatus] = useState<KeyStatus | null>(null), [key,setKey] = useState(''), [replacing,setReplacing] = useState(false);
  const [agent,setAgent] = useState(agents[0]?.id ?? ''), [search,setSearch] = useState(''), [query,setQuery] = useState(''), [category,setCategory] = useState(''), [cursors,setCursors] = useState(['']);
  const [refresh,setRefresh] = useState(0), [error,setError] = useState(''), [busy,setBusy] = useState(false), [link,setLink] = useState('');
  const [setup, setSetup] = useState(false), [selected, setSelected] = useState('');
  useEffect(() => { if (selected) heading.current?.focus(); }, [selected]);
  const keySave=pending?.path==='/api/connected-apps/key', locked = busy || !!pending || invalid, after=cursors.at(-1)!;
  useEffect(() => { mounted.current=true; heading.current?.focus(); return ()=>{mounted.current=false;}; },[]);
  const loadScope = useRef(''), [loadError, setLoadError] = useState('');
  usePolling(async signal => {
    const scope = JSON.stringify([agent, query, category, after, refresh]);
    if (loadScope.current !== scope) { loadScope.current = scope; setCatalog(null); setAccounts(null); setKeyStatus(null); setGrants(null); setPolicies(null); }
    const params = new URLSearchParams({ search: query, category, after });
    try {
      const [status, catalog, accounts, grants, policies] = await Promise.all([request<KeyStatus>('/api/connected-apps/key').then(status => { if (!signal.aborted) setKeyStatus(status); return status; }), request<Catalog>(`/api/connected-apps?${params}`), request<{ items: Account[] }>('/api/connected-apps/accounts'), agent ? request<{ items: Grant[] }>(`/api/agents/${agent}/connected-apps`) : Promise.resolve({ items: [] }), agent ? request<{ items: Policy[] }>(`/api/agents/${agent}/connected-app-policies`) : Promise.resolve({ items: [] })]);
      if (!signal.aborted) { setKeyStatus(status); setCatalog(catalog); setAccounts(accounts.items); setGrants(grants.items); setPolicies(policies.items); setLoadError(''); }
    } catch (error) { if (!signal.aborted) setLoadError(error instanceof Error ? error.message : 'Connected apps could not load.'); }
  }, { interval: 15000 }, [agent, query, category, after, refresh, request]);
  async function execute(command:ConnectedAppCommand,connect=false) {
    if(sending.current || invalid)return;
    if(command.path==='/api/connected-apps/key' && !key.trim()){setError('Paste the Composio API key again to retry this save.');return;}
    let popup:Window|null=null;
    if(connect){popup=window.open('about:blank','_blank'); if(popup)popup.opener=null;}
    try {
      restoreConnectedAppCommand(JSON.stringify(command)); sessionStorage.setItem(storage,JSON.stringify(command)); setPending(command); sending.current=true; setBusy(true); setError('');
      const result=await request<{redirect_url?:string}>(command.path,command.path==='/api/connected-apps/key'?{...command.body,key}:command.body);
      if(result.redirect_url){if(popup)popup.location.href=result.redirect_url; if(mounted.current)setLink(result.redirect_url);} else popup?.close();
      if(sessionStorage.getItem(storage)===JSON.stringify(command))sessionStorage.removeItem(storage);
      if(mounted.current){if(command.path.startsWith('/api/connected-apps/key')){setKey('');setReplacing(false);setSetup(false);setLink('');}setPending(null);setRefresh(n=>n+1);}
    }catch(error){popup?.close();if(mounted.current)setError(error instanceof Error?error.message:'Change is not confirmed. Retry the saved request.');}
    finally{sending.current=false;if(mounted.current)setBusy(false);}
  }
  async function discard(){if(!await confirm({title:'Discard saved request?',description:'The change may already have succeeded. Connected accounts and access reload before another change.',confirm:'Discard request',danger:true}))return;try{sessionStorage.removeItem(storage);setPending(null);setInvalid(false);setRefresh(n=>n+1);}catch{setError('Browser recovery data could not be removed.');}}
  async function saveKey() {
    if(sending.current || invalid || !keyStatus || !key.trim() || pending && !keySave)return;
    await execute(pending ?? {path:'/api/connected-apps/key',body:{id:crypto.randomUUID(),revision:keyStatus.revision}});
  }
  async function removeKey() {
    if(sending.current || locked || keyStatus?.source!=='settings')return;
    if(!await confirm({title:'Remove Composio API key?',description:'Connected accounts, agent access, pending approvals, and saved "Always allow" choices will be reset. Existing accounts remain in the old Composio project.',confirm:'Remove key',danger:true}))return;
    await execute({path:'/api/connected-apps/key/remove',body:{id:crypto.randomUUID(),revision:keyStatus.revision}});
  }
  const title=(slug:string)=>catalog?.items.find(row=>row.slug===slug)?.name ?? ({gmail:'Gmail',slack:'Slack',github:'GitHub',googlecalendar:'Google Calendar'}[slug] ?? slug);
  const selectedGrant = grants?.find(row => row.toolkit === selected);
  const connected=[...new Set(accounts?.filter(row=>row.status==='ACTIVE').map(row=>row.toolkit) ?? [])];
  const recoveryNotice = <>
    {invalid && <Alert tone="error" title="Recovery unavailable" actions={<Button size="sm" onClick={()=>void discard()}>Discard request</Button>} />}
    {pending && <RecoveryAlert inFlight={busy} tone="warn" title="Not confirmed" actions={<>
      {keySave && !project && <Button size="sm" onClick={()=>setSetup(true)}>Enter key</Button>}
      <Button size="sm" busy={busy} disabled={keySave && !key.trim()} onClick={()=>void execute(pending,pending.path.includes('/connect/'))}>Try again</Button><Button size="sm" disabled={busy} onClick={()=>void discard()}>Discard request</Button>
    </>}>{keySave?'Enter the same API key to retry. The key was not saved in browser storage.':undefined}</RecoveryAlert>}
  </>;
  const openSetup = async () => {
    if(sending.current || locked)return;
    if(keyStatus?.source==='settings') {
      if(!await confirm({title:'Replace Composio API key?',description:'Connected accounts, agent access, pending approvals, and saved "Always allow" choices will be reset. Existing accounts remain in the old Composio project.',confirm:'Replace key',danger:true}))return;
      setReplacing(true);
    }
    setSetup(true);
  };
  return <div className="k-page bs-root bs-set3-apps">
    <style nonce={getNonce()}>{`.bs-root.bs-set3-apps .k-row__title { white-space: normal; overflow-wrap: anywhere; } .bs-root.bs-set3-apps .k-row__meta { overflow-wrap: anywhere; }`}</style>
    {selected && <Button aria-label="Back to Connected apps" variant="ghost" icon={<ArrowLeft size={14} aria-hidden />} onClick={()=>{setSelected('');heading.current?.focus();}}>Connected apps</Button>}
    <PageHeader headingRef={heading} title={selected ? title(selected) : 'Connected apps'} actions={<>
      {!project && <ActionMenu label="Integration setup" trigger={<IconButton label="Integration setup" tooltip="More actions" disabled={busy}><Ellipsis size={16} aria-hidden /></IconButton>} items={[
        {label:keyStatus?.source==='settings'?'Replace key':'Set up',disabled:locked||!keyStatus,onSelect:()=>void openSetup()},
        ...(keyStatus?.source==='settings' ? ['separator' as const,{label:'Remove key',danger:true,disabled:locked,onSelect:()=>void removeKey()}] : []),
      ]} />}
    </>} />
    {!project && !selected && <WhatsAppSettings request={request} agents={agents} sections={sections} />}
    {(error || loadError) && <Alert tone="error" actions={loadError ? <Button size="sm" onClick={() => setRefresh(n => n + 1)}>Try again</Button> : undefined}>{error || loadError}</Alert>}{recoveryNotice}
    {project && !selected && <Card><Row title="Allow global apps" trailing={<Tooltip content="Member agents may also use apps granted to them in Settings. Project accounts take priority for the same toolkit."><span><Switch label="Allow global apps" checked={project.allowInstallationApps} disabled={locked} onChange={async enabled=>{if(enabled&&!await confirm({title:'Allow installation apps in this project?',description:'Members can use their installation accounts alongside this project’s accounts. Approval choices stay separate for this project.',confirm:'Allow installation apps'}))return;try{await onInstallationAppsChange?.(enabled);}catch(error){setError(error instanceof Error?error.message:'Project setting could not save.');}}}/></span></Tooltip>} /></Card>}
    {catalog?.configured===false ? <EmptyState icon={<Blocks size={20} aria-hidden />} title="Connect apps" action={!project ? <Button disabled={locked||!keyStatus} onClick={()=>void openSetup()}>Set up</Button> : undefined}>{project?'Set up Composio in Settings > Connected apps.':undefined}</EmptyState> : selected ? <>
      <Section title="Accounts">
        {accounts === null ? !error && !loadError && <SkeletonRows count={1} /> : !accounts.some(row=>row.toolkit===selected) ? <EmptyState title="No connected accounts" /> : <Card>{accounts.filter(row=>row.toolkit===selected).map(row=><Row key={row.id} title={row.id} meta={<Badge tone={row.status==='ACTIVE'?'success':'neutral'}>{row.status==='ACTIVE'?'Connected':row.status}</Badge>} trailing={<ActionMenu label={`Manage account ${row.id}`} items={[{label:'Disconnect',danger:true,onSelect:async()=>{if(await confirm({title:`Disconnect ${title(row.toolkit)}?`,description:'Agent access is revoked. Completed app actions cannot be undone.',confirm:'Disconnect',danger:true}))void execute({path:`/api/connected-apps/disconnect/${row.id}`,body:{id:crypto.randomUUID()}});}}]} trigger={<IconButton label={`Manage account ${row.id}`} tooltip="More actions" disabled={locked}><Ellipsis size={16} aria-hidden /></IconButton>} />} />)}</Card>}
      </Section>
      <Section title="Agent access" actions={!!agents.length && <Select size="sm" label="Agent for connected apps" value={agent} disabled={locked} onChange={setAgent} options={agents.map(row=>({value:row.id,label:row.name}))} />}>
        {!agent ? <EmptyState title="No agents yet" /> : grants===null ? !error && !loadError && <SkeletonRows count={1} /> : !connected.includes(selected) ? <EmptyState title="No connected account" /> : <Card><Row title={title(selected)} trailing={<Switch label={`Allow ${agents.find(row=>row.id===agent)?.name} to use ${title(selected)}`} checked={!!selectedGrant?.enabled} disabled={locked} onChange={enabled=>void execute({path:`/api/agents/${agent}/connected-apps/${selected}`,body:{id:crypto.randomUUID(),revision:selectedGrant?.revision ?? 0,enabled}})} />} /></Card>}
        {project && connected.includes(selected) && <Button size="sm" disabled={locked || !agents.length} onClick={async()=>{if(await confirm({title:`Allow all project members to use ${title(selected)}?`,description:`This grants ${agents.length} current members access. Actions that write or have unknown effects still ask for approval in chat.`,confirm:'Allow all members'}))void execute({path:`/api/connected-apps/grant-all/${selected}`,body:{id:crypto.randomUUID(),confirmed:true}});}}>Allow all members</Button>}
      </Section>
      <Section title="Allowed actions">
        {policies===null ? !error && !loadError && <SkeletonRows count={1} /> : !policies.some(row=>row.toolkit===selected) ? <EmptyState title="No saved approvals" /> : <Card>{policies.filter(row=>row.toolkit===selected).map(row=><Row key={row.fingerprint} title={row.tool} meta={row.account} trailing={<Button size="sm" disabled={locked} onClick={()=>void execute({path:`/api/agents/${agent}/connected-app-policies`,body:{id:crypto.randomUUID(),fingerprint:row.fingerprint}})}>Ask next time</Button>} />)}</Card>}
      </Section>
    </> : <>
      {link && <Alert title="Finish connecting in Composio" actions={<Button size="sm" onClick={()=>setRefresh(n=>n+1)}>Check connection</Button>}><a href={link} target="_blank" rel="noopener noreferrer">Open connection page</a></Alert>}
      <Section title="Accounts">
        {accounts===null ? !error && !loadError && <><span role="status" className="sr-only">Loading connected apps...</span><SkeletonRows count={2} /></> : !accounts.length ? <EmptyState title="No connected accounts" /> : <Card>{accounts.map(row=><Row key={row.id} title={title(row.toolkit)} onClick={()=>setSelected(row.toolkit)} meta={<><span>{row.id}</span><Badge tone={row.status==='ACTIVE'?'success':'neutral'}>{row.status==='ACTIVE'?'Connected':row.status}</Badge></>} />)}</Card>}
      </Section>
      <Section title="Browse apps">
        <form className="k-inline" style={{alignItems:'flex-end'}} onSubmit={event=>{event.preventDefault();setCursors(['']);setQuery(search);}}><Field label="Search apps"><Input type="search" maxLength={100} placeholder="Gmail, Slack, GitHub..." value={search} disabled={locked} onChange={event=>setSearch(event.target.value)} /></Field><Button type="submit" icon={<Search size={14} aria-hidden />} disabled={locked}>Search</Button></form>
        {!!catalog && <div className="k-row-actions" aria-label="App categories"><Button size="sm" variant={category===''?'brand':'ghost'} aria-pressed={category===''} disabled={locked} onClick={()=>{setCategory('');setCursors(['']);}}>All apps</Button>{catalog.categories.map(row=><Button key={row.id} size="sm" variant={category===row.id?'brand':'ghost'} aria-pressed={category===row.id} disabled={locked} onClick={()=>{setCategory(row.id);setCursors(['']);}}>{row.name}</Button>)}</div>}
        {!catalog ? !error && !loadError && <SkeletonRows /> : !catalog.items.length ? <EmptyState title="No apps found" /> : <Card>{catalog.items.map(app=>{
          const rows=accounts?.filter(row=>row.toolkit===app.slug) ?? [], active=rows.some(row=>row.status==='ACTIVE');
          return <Row key={app.slug} leading={<Blocks size={16} aria-hidden />} title={app.name} meta={active ? <Badge tone="success">Connected</Badge> : undefined} onClick={active?()=>setSelected(app.slug):undefined} trailing={active ? <Button size="sm" disabled={locked} onClick={()=>setSelected(app.slug)}>Manage</Button> : <Button size="sm" disabled={locked || !app.managedAuth} onClick={()=>void execute({path:`/api/connected-apps/connect/${app.slug}`,body:{id:crypto.randomUUID()}},true)}>{app.managedAuth?'Connect':'Custom auth needed'}</Button>} />;
        })}</Card>}
        {(cursors.length>1 || catalog?.next_after) && <div className="k-pager"><IconButton label="Previous apps" disabled={locked || cursors.length<2} onClick={()=>setCursors(rows=>rows.slice(0,-1))}><ChevronLeft size={16} aria-hidden /></IconButton><IconButton label="Next apps" disabled={locked || !catalog?.next_after} onClick={()=>{setCursors(rows=>[...rows,catalog!.next_after!]);heading.current?.focus();}}><ChevronRight size={16} aria-hidden /></IconButton></div>}
      </Section>
    </>}
    {!project && <Dialog open={setup} onOpenChange={open=>{setSetup(open);if(!open&&!pending){setReplacing(false);setKey('');}}} title={replacing?'Replace key':'Set up Composio'} footer={keyStatus?.source!=='environment' || keySave ? <Button type="submit" form="composio-key" variant="primary" busy={busy} disabled={busy || invalid || !key.trim() || !!pending && !keySave}>{keySave?'Try again':'Save key'}</Button> : undefined}>
      <div className="k-stack">{(error || loadError) && <Alert tone="error" actions={loadError ? <Button size="sm" onClick={() => setRefresh(n => n + 1)}>Try again</Button> : undefined}>{error || loadError}</Alert>}{recoveryNotice}
      {!keyStatus ? <SkeletonRows count={1} /> : keyStatus.source==='environment' && !keySave ? <Badge>From server environment</Badge> : <form id="composio-key" onSubmit={event=>{event.preventDefault();void saveKey();}}>
        <Field label="Composio API key"><Input type="password" autoComplete="off" spellCheck={false} maxLength={4096} value={key} disabled={busy || invalid} onChange={event=>setKey(event.target.value)} /></Field>
      </form>}
      <a href="https://platform.composio.dev/" target="_blank" rel="noreferrer">Get API key</a>
      </div>
    </Dialog>}
  </div>;
}

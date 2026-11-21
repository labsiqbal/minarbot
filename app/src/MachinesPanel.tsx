import { isReconnecting } from './http';
import { RightPanel, useRightPanel } from './RightPanel';
import { useDetailsOpen } from './ui/layout';
import { useCachedData, useQueryPolling } from './hooks/useCachedQuery';
import { queryCache } from './hooks/queryCache';
import { usePolling } from './hooks/usePolling';
import { copyText } from './ui/clipboard';
import { useEffect, useRef, useState } from 'react';
import { Check, ChevronDown, ChevronRight, Copy, Ellipsis, Monitor, Plus } from 'lucide-react';
import type { Environment, PairedMachine, PeerAgentSummary } from '../shared/machines';
import { ActionMenu, Alert, Badge, Button, Card, Dialog, EmptyState, Field, IconButton, Input, PageHeader, Row, Section, SkeletonRows, Switch, Textarea, Tooltip, useConfirm } from './ui/kit';
import { Markdown } from './Markdown';
import { restoreRemoteMessage } from './remote-message-command';
import { TurnActivity } from './ui/TurnActivity';
import { ChatComposer, ChatEmptyState, ChatHeader, ChatTranscript, HumanMessage, AgentMessage, TurnStatus } from './ui/ChatPrimitives';
import './ui/machines.css';
import './machines-data.css';
import { HostMetricsSection } from './HostMetricsSection';

type Request = <T>(path: string, data?: unknown) => Promise<T>;
const message = (error: unknown) => error instanceof Error ? error.message : 'Machine request failed. Try again.';
const statusLabel = { online: 'Online', offline: 'Offline', unauthorized: 'Unauthorized', 'version-mismatch': 'Version mismatch' };
const machinesPath = '/api/machines', environmentPath = '/api/environment';
type MachineList = { machines: PairedMachine[] };
export function useMachines(request: Request) {
  const { data } = useCachedData<MachineList>(machinesPath);
  const [failure, setFailure] = useState<unknown>(null);
  const generation = useRef(0);
  async function reload(signal?: AbortSignal) {
    const current = ++generation.current;
    try {
      await queryCache.refresh(machinesPath, () => request<MachineList>(machinesPath));
      if (!signal?.aborted && current === generation.current) {
        setFailure(null);
      }
    } catch (error) { if (!signal?.aborted && current === generation.current) setFailure(error); }
  }
  useEffect(() => () => { generation.current++; }, []);
  useQueryPolling(signal => reload(signal), { interval: 5000 });
  return { machines: data?.machines ?? [], error: failure ? message(failure) : '', reconnecting: isReconnecting(failure), reload, loaded: data !== null };
}
export function MachinesPanel({ request, onChanged }: { request: Request; onChanged: () => void }) {
  const { data: environment } = useCachedData<Environment>(environmentPath);
  const { data } = useCachedData<MachineList>(machinesPath);
  const machines = data?.machines ?? null;
  const [draftName, setName] = useState<string>();
  const name = draftName ?? environment?.name ?? '';
  const [origin, setOrigin] = useState(''), [code, setCode] = useState('');
  const [pairingCode, setPairingCode] = useState<{ code: string; expiresAt: number } | null>(null), [now, setNow] = useState(Date.now());
  const [busy, setBusy] = useState(''), [error, setError] = useState(''), [notice, setNotice] = useState('');
  const [setup, setSetup] = useState<'install' | 'pair' | 'code' | null>(null), [detail, setDetail] = useState<string | null>(null), [install, setInstall] = useState<{ source: string; token: string; expiresAt: number } | null>(null), [installError, setInstallError] = useState(''), [installBusy, setInstallBusy] = useState(false), [copied, setCopied] = useState(false);
  const addTrigger = useRef<HTMLButtonElement>(null);
  const heading = useRef<HTMLHeadingElement>(null), pendingAdd = useRef<{ id: string; origin: string; code: string } | null>(null);
  const confirm = useConfirm();
  const [loadError, setLoadError] = useState('');
  async function reload(signal?: AbortSignal) { try { await Promise.all([
      queryCache.refresh(environmentPath, () => request<Environment>(environmentPath)),
      queryCache.refresh(machinesPath, () => request<MachineList>(machinesPath)),
    ]); if (!signal?.aborted) setLoadError(''); } catch (error) { if (!signal?.aborted) setLoadError(message(error)); } }
  useEffect(() => { heading.current?.focus(); }, []);
  useQueryPolling(signal => reload(signal), { interval: 5000, enabled: !busy }, [request]);
  useEffect(() => { if (!pairingCode) return; const timer = setInterval(() => { const time = Date.now(); setNow(time); if (time >= pairingCode.expiresAt) setPairingCode(null); }, 1000); return () => clearInterval(timer); }, [pairingCode]);
  useEffect(() => { if (!install) return; const timer = setInterval(() => setNow(Date.now()), 1000); return () => clearInterval(timer); }, [install]);
  const installFlight = useRef(0);
  function closeInstall(open: boolean) { setSetup(open ? 'install' : null); if (!open) { installFlight.current++; setInstall(null); setInstallError(''); setInstallBusy(false); setCopied(false); } }
  async function createInstall() {
    const flight = ++installFlight.current; setInstallBusy(true); setInstallError(''); setCopied(false);
    try { const value = await request<{ source: string; token: string; expiresAt: number }>('/api/install/tokens', { id: crypto.randomUUID() }); if (flight === installFlight.current) { setInstall(value); setNow(Date.now()); } }
    catch (error) { if (flight === installFlight.current) setInstallError(message(error)); }
    finally { if (flight === installFlight.current) setInstallBusy(false); }
  }
  async function act(action: string, run: () => Promise<void>) { setBusy(action); setError(''); setNotice(''); try { await run(); onChanged(); } catch (error) { setError(message(error)); } finally { setBusy(''); } }
  async function unpair(machine: PairedMachine) {
    if (!await confirm({ title: `Unpair ${machine.name}?`, description: 'Removes this machine from your list and revokes its access here. Minarbot also asks the other machine to revoke access. If it is offline, remove this pairing there too.', confirm: 'Unpair machine', danger: true })) return;
    await act('unpair', async () => { await request('/api/machines/remove', { id: crypto.randomUUID(), environmentId: machine.id }); await reload(); setDetail(null); setNotice(`${machine.name} was unpaired.`); });
  }
  const remaining = pairingCode ? Math.max(0, Math.ceil((pairingCode.expiresAt - now) / 1000)) : 0;
  const installRemaining = install ? Math.max(0, Math.ceil((install.expiresAt - now) / 1000)) : 0;
  const shellQuote = (value: string) => `'${value.replaceAll("'", "'\\''")}'`;
  const installCommand = install && installRemaining ? `curl -fsSL ${shellQuote(install.source + '/api/install/script')} | sh -s -- --source ${shellQuote(install.source)} --token ${shellQuote(install.token)} --serve` : '';
  const peer = machines?.find(machine => machine.id === detail);
  function closeSetup(open: boolean) { if (!open && !busy) { setSetup(null); setPairingCode(null); setCopied(false); } }
  function openSetup(flow: 'install' | 'pair' | 'code') { requestAnimationFrame(() => { addTrigger.current?.focus(); if (flow === 'install') closeInstall(true); else setSetup(flow); }); }
  const addMachine = <ActionMenu label="Add machine" trigger={<Button ref={addTrigger} variant="primary" icon={<Plus size={14} aria-hidden />}>Add machine<ChevronDown size={14} aria-hidden /></Button>} items={[
    { label: 'Install Minarbot', onSelect: () => openSetup('install') },
    { label: 'Pair machine', onSelect: () => openSetup('pair') },
    { label: 'Show pairing code', onSelect: () => openSetup('code') },
  ]} />;
  return <div className="k-page bs-root machines-page set4-page">
    <PageHeader headingRef={heading} title="Machines" actions={<>{addMachine}</>} />
    {(error || loadError) && <Alert tone="error" actions={loadError && <Button size="sm" onClick={() => void reload()}>Retry</Button>}>{error || loadError}</Alert>}<span className="sr-only" role="status">{notice}</span>
    <Section title="Paired machines">
      {machines === null ? !error && <SkeletonRows /> : !machines.length ? <EmptyState icon={<Monitor size={18} />} title="No paired machines" /> : <Card>{machines.map(machine => <Row key={machine.id} leading={<Monitor size={16} aria-hidden />} title={machine.name} onClick={() => setDetail(machine.id)} meta={<><Badge tone={machine.status === 'online' ? 'success' : 'warn'}>{statusLabel[machine.status]}</Badge><span>{machine.agentCount} {machine.agentCount === 1 ? 'agent' : 'agents'}</span>{machine.pairingState === 'pending' && <Badge tone="warn">Pairing incomplete</Badge>}</>} trailing={<><ActionMenu label={`Machine actions: ${machine.name}`} trigger={<IconButton label={`Machine actions: ${machine.name}`} disabled={!!busy}><Ellipsis size={16} aria-hidden /></IconButton>} items={[{ label: `Unpair ${machine.name}`, danger: true, onSelect: () => { requestAnimationFrame(() => { document.querySelector<HTMLButtonElement>(`[aria-label="Machine actions: ${CSS.escape(machine.name)}"]`)?.focus(); void unpair(machine); }); } }]} /><IconButton label={`Open ${machine.name}`} onClick={() => setDetail(machine.id)}><ChevronRight size={16} aria-hidden /></IconButton></>} />)}</Card>}
    </Section>
    <Section title="This device"><Card><Row leading={<Monitor size={16} aria-hidden />} title={environment?.name ?? 'This device'} meta="This device" onClick={() => setDetail('local')} trailing={<Button variant="ghost" onClick={() => setDetail('local')}>Resources<ChevronRight size={14} aria-hidden /></Button>} /></Card></Section>
    <Dialog open={detail === 'local'} onOpenChange={open => { if (!open) setDetail(null); }} title="This device" size="lg">
      <div className="k-stack set4-detail">
        {!environment ? <SkeletonRows count={1} /> : <form className="k-stack" onSubmit={event => { event.preventDefault(); void act('name', async () => { const saved = await request<Environment>('/api/environment', { id: crypto.randomUUID(), name }); queryCache.set(environmentPath, saved); setName(undefined); setNotice('Machine name saved.'); }); }}>
          <Field label="Name"><Input value={name} maxLength={120} required disabled={!!busy} onChange={event => setName(event.target.value)} /></Field>
          <div className="machines-form-actions"><span className="k-field__hint machines-origin">{environment.origin}</span><Button type="submit" busy={busy === 'name'} disabled={!!busy || name.trim() === environment.name}>Save name</Button></div>
        </form>}
        {error && <Alert tone="error">{error}</Alert>}
        {detail === 'local' && <HostMetricsSection request={request} />}
      </div>
    </Dialog>
    <Dialog open={!!peer} onOpenChange={open => { if (!open) setDetail(null); }} title={peer?.name ?? 'Machine'} footer={<Button variant="danger" disabled={!!busy} onClick={() => { if (peer) void unpair(peer); }}>Unpair machine</Button>}>
      {peer && <div className="k-stack set4-detail">
        <Card><Row title="Address" trailing={<span className="machines-origin">{peer.origin}</span>} /><Row title="Status" trailing={<Badge tone={peer.status === 'online' ? 'success' : 'warn'}>{statusLabel[peer.status]}</Badge>} /><Row title="Version" trailing={peer.version} /><Row title="Agents" trailing={peer.agentCount} />{peer.pairingState === 'pending' && <Row title="Pairing" trailing={<Tooltip content={<span className="bs-root set4-tooltip">{"The other machine has not confirmed return access."}</span>}><span tabIndex={0}><Badge tone="warn">Pairing incomplete</Badge></span></Tooltip>} />}<Row title="Last seen" trailing={peer.lastSeen ? new Date(peer.lastSeen).toLocaleString() : 'Not seen yet'} /></Card>
        <Card><Row title={<Tooltip content={<span className="bs-root set4-tooltip">{`Allow ${peer.name} agents to message agents here.`}</span>}><span tabIndex={0}>Incoming messages</span></Tooltip>} trailing={<Switch label="Incoming messages" checked={peer.allowAgentMessaging} disabled={!!busy} onChange={allowed=>void act('policy',async()=>{await request('/api/machines/agent-messaging',{id:crypto.randomUUID(),environmentId:peer.id,allowed});await reload();})} />} /><Row title={<Tooltip content={<span className="bs-root set4-tooltip">{`Allow updates from ${peer.name}. Enable on both machines.`}</span>}><span tabIndex={0}>Remote updates</span></Tooltip>} trailing={<Switch label="Remote updates" checked={peer.allowUpdates} disabled={!!busy} onChange={allowed=>void act('updates',async()=>{await request('/api/machines/updates',{id:crypto.randomUUID(),environmentId:peer.id,allowed});await reload();})} />} /></Card>
        <Section title="Provider accounts">
          {peer.providerAccounts?.length
            ? <Card>{peer.providerAccounts.map(account => <Row key={account.id} title={account.name} meta={<span className="machines-origin">{account.host}</span>} trailing={account.defaultModel ? <code>{account.defaultModel}</code> : 'No default model'} />)}</Card>
            : <EmptyState title={peer.providerAccounts ? 'No provider accounts' : 'Provider accounts unavailable'} />}
        </Section>
        {error && <Alert tone="error">{error}</Alert>}
      </div>}
    </Dialog>
    <Dialog open={setup === 'pair'} onOpenChange={closeSetup} title="Pair machine" footer={<><Button disabled={!!busy} onClick={() => closeSetup(false)}>Cancel</Button><Button form="set4-pair" type="submit" variant="primary" busy={busy === 'add'} disabled={!!busy}>Pair machine</Button></>}>
      <form id="set4-pair" className="k-stack set4-detail" onSubmit={event => { event.preventDefault(); const command = pendingAdd.current ?? { id: crypto.randomUUID(), origin, code: code.trim().toUpperCase() }; pendingAdd.current = command; void act('add', async () => { await request('/api/machines', command); pendingAdd.current = null; setCode(''); setOrigin(''); await reload(); setSetup(null); setNotice('Machines are paired in both directions.'); }); }}>
        <Field label={<Tooltip content={<span className="bs-root set4-tooltip">{"Private HTTPS URL, including port"}</span>}><span tabIndex={0}>Address</span></Tooltip>}><Input type="url" required placeholder="https://workstation.example.test:4317" value={origin} maxLength={2048} disabled={!!busy} onChange={event => { setOrigin(event.target.value); pendingAdd.current = null; }} /></Field>
        <Field label="Pairing code"><Input required value={code} minLength={12} maxLength={12} autoComplete="off" spellCheck={false} disabled={!!busy} onChange={event => { setCode(event.target.value.toUpperCase()); pendingAdd.current = null; }} /></Field>
        {error && <Alert tone="error">{error}</Alert>}
      </form>
    </Dialog>
    <Dialog open={setup === 'code'} onOpenChange={closeSetup} title="Show pairing code" footer={<><Button disabled={!!busy} onClick={() => closeSetup(false)}>Close</Button>{!pairingCode && <Button variant="primary" disabled={!!busy} busy={busy === 'code'} onClick={() => void act('code', async () => { const result = await request<{ code: string; expiresAt: number }>('/api/pairing-codes', { id: crypto.randomUUID() }); setNow(Date.now()); setCopied(false); setPairingCode(result); })}>Create code</Button>}</>}>
      <div className="k-stack set4-detail"><Field label="Address"><Input readOnly value={environment?.origin ?? ''} /></Field>
        {pairingCode ? <div className="machines-code" role="region" aria-label="One-time pairing code"><span className="machines-code__value">{pairingCode.code}</span><IconButton label={copied ? 'Copied' : 'Copy pairing code'} onClick={async () => { try { await copyText(pairingCode.code); setError(''); setCopied(true); } catch { setCopied(false); setError('Copy failed. Select the code and copy it manually.'); } }}>{copied ? <Check size={15} aria-hidden /> : <Copy size={15} aria-hidden />}</IconButton><Tooltip content={<span className="bs-root set4-tooltip">Single use. Closing hides this code.</span>}><span className="k-field__hint" tabIndex={0}>Expires in <span className="machines-countdown">{Math.floor(remaining / 60)}:{String(remaining % 60).padStart(2, '0')}</span></span></Tooltip><Button size="sm" onClick={() => { setPairingCode(null); setCopied(false); }}>Hide code</Button></div> : <EmptyState title="No active code" />}
        {error && <Alert tone="error">{error}</Alert>}
      </div>
    </Dialog>
    <Dialog open={setup === 'install'} onOpenChange={closeInstall} title="Install Minarbot" footer={<><Button onClick={() => closeInstall(false)}>Close</Button>{!installCommand && <Button variant="primary" busy={installBusy} onClick={() => void createInstall()}>{install ? 'Create new command' : 'Create install command'}</Button>}</>}>
      <div className="k-stack machine-install set4-detail">
        <Tooltip content={<span className="bs-root set4-tooltip">{"Requires Node.js 22.23.4 or later, a systemd user session, and Tailscale on the same tailnet with MagicDNS and HTTPS enabled. Git and Bubblewrap are optional."}</span>}><span className="k-field__hint" tabIndex={0}>Linux · Requirements</span></Tooltip>
        <Tooltip content={<span className="bs-root set4-tooltip">{"Installs a private release, starts a user service, enables linger if permitted, and uses Tailscale Serve on port 4317."}</span>}><span className="k-field__hint" tabIndex={0}>User service · Tailscale Serve :4317</span></Tooltip>
        {installError && <Alert tone="error">{installError}</Alert>}
        {installCommand ? <>
          <Field label="Install command"><Textarea readOnly className="machine-install__command" value={installCommand} spellCheck={false} autoComplete="off" /></Field>
          <div className="machines-form-actions"><Tooltip content={<span className="bs-root set4-tooltip">{"Grants one release download and pairing code. Closing hides the command. Provider credentials stay on each machine."}</span>}><span className="k-field__hint" tabIndex={0}>Private, single use</span></Tooltip><span className="k-field__hint">Expires in <span className="machines-countdown">{Math.floor(installRemaining / 60)}:{String(installRemaining % 60).padStart(2, '0')}</span></span><Button onClick={async () => { try { await copyText(installCommand); setInstallError(''); setCopied(true); } catch { setCopied(false); setInstallError('Copy failed. Select the command and copy it manually.'); } }}>{copied ? 'Copied' : 'Copy command'}</Button></div>
          <span className="sr-only" role="status">{copied ? 'Command copied.' : ''}</span>
        </> : install && <Alert tone="warn">This install command expired. Create a new one to continue.</Alert>}
      </div>
    </Dialog>
  </div>;
}
type RemoteMessage = {id:string;text:string;status:string;state:string;reply:string|null;error:string|null;created:number};
function remoteStatus(item: RemoteMessage) { return item.state === 'uncertain' || item.status === 'interrupted' ? 'Uncertain' : item.status === 'done' ? 'Done' : item.status === 'running' ? 'Replying' : item.status === 'failed' ? 'Failed' : item.status === 'cancelled' ? 'Cancelled' : item.status === 'waiting' ? 'Waiting' : 'Queued'; }
function remoteTone(item: RemoteMessage) { return item.state === 'uncertain' || ['failed', 'interrupted'].includes(item.status) ? 'error' : item.status === 'waiting' ? 'attention' : ['queued', 'running'].includes(item.status) ? 'working' : 'done'; }
export function RemoteAgentDetail({ machine, agent, request }: { machine?: PairedMachine; agent?: PeerAgentSummary; request: Request }) {
  const rightPanel = useRightPanel(`remote:${machine?.id}:${agent?.id}`);
  const [detailsOpen, setDetailsOpen] = useDetailsOpen();
  const storage=`minarbot:remote-message:${machine?.id}:${agent?.id}`;
  const [recovery]=useState(()=>{try{return {pending:restoreRemoteMessage(sessionStorage.getItem(storage),machine?.id ?? '',agent?.id ?? ''),draft:sessionStorage.getItem(storage+':draft') ?? '',invalid:false};}catch{return {pending:null,draft:'',invalid:true};}});
  const [pending,setPending]=useState(recovery.pending),[invalid,setInvalid]=useState(recovery.invalid);
  const [messages,setMessages]=useState<RemoteMessage[]|null>(null),[text,setText]=useState(recovery.pending?.text ?? recovery.draft),[error,setError]=useState(''),[loadError,setLoadError]=useState(''),[busy,setBusy]=useState(false);
  const end=useRef<HTMLDivElement>(null),readEpoch=useRef(0),flight=useRef(false),mounted=useRef(true),confirm=useConfirm();
  useEffect(()=>{mounted.current=true;return()=>{mounted.current=false;};},[]);
  const path=`/api/remote-agents/messages?machine=${machine?.id ?? ''}&agent=${agent?.id ?? ''}`;
  useEffect(() => { setMessages(null); return () => { readEpoch.current++; }; }, [path]);
  usePolling(async signal => {if(!machine||!agent)return;const epoch=++readEpoch.current;try{const result=await request<{messages:RemoteMessage[]}>(path);if(!signal.aborted&&epoch===readEpoch.current){setMessages(result.messages);setLoadError('');}}catch(e){if(!signal.aborted&&epoch===readEpoch.current)setLoadError(message(e));}}, { interval: 1000 }, [path]);
  useEffect(()=>{end.current?.scrollIntoView({block:'nearest'});},[messages?.length,messages?.at(-1)?.reply]);
  async function send(){
    if(!machine||!agent||flight.current||invalid||disabledReason||!text.trim())return;
    const command=pending ?? {id:crypto.randomUUID(),machine:machine.id,agent:agent.id,text:text.trim()};
    flight.current=true;readEpoch.current++;setBusy(true);setError('');
    try{
      sessionStorage.setItem(storage,JSON.stringify(command));setPending(command);
      await request('/api/remote-agents/messages',command);
      sessionStorage.removeItem(storage);sessionStorage.removeItem(storage+':draft');
      if(!mounted.current)return;
      setPending(null);setText('');const epoch=++readEpoch.current,result=await request<{messages:RemoteMessage[]}>(path);if(mounted.current&&epoch===readEpoch.current)setMessages(result.messages);
    }catch(e){if(mounted.current)setError(message(e));}finally{flight.current=false;if(mounted.current)setBusy(false);}
  }
  async function discard(){if(!await confirm({title:'Discard saved message?',description:'Delivery may already have succeeded. Check this conversation and the receiving machine before sending a replacement.',confirm:'Discard message',danger:true}))return;try{sessionStorage.removeItem(storage);setPending(null);setInvalid(false);}catch{setError('Browser storage could not be updated.');}}
  const heading = useRef<HTMLDivElement>(null); useEffect(() => { heading.current?.focus(); }, [machine?.id, agent?.id]);
  const binding = agent && machine?.agentProviders?.[agent.id];
  const provider = machine?.providerAccounts?.find(account => account.id === binding?.provider_account);
  const disabledReason = invalid ? 'Saved message could not be read. Discard it after checking conversation history.' : machine?.status !== 'online' ? `${machine ? statusLabel[machine.status] : 'Remote machine unavailable'}. Reconnect the machine before sending.` : machine?.remoteAgentMessaging === false ? `Messaging is turned off on ${machine.name}. Enable it in Settings > Machines on that machine.` : binding?.runnable === false ? binding.blocked_reason ?? undefined : undefined;
  return <><main className="chat bs-root remote-conversation" aria-label={machine && agent ? `Conversation ${agent.name} on ${machine.name}` : 'Remote agent unavailable'}>
    <ChatHeader agent={agent ?? { id: '', name: 'Remote agent' }} title={agent?.name ?? 'Remote agent unavailable'} pillRef={heading} tooltip={machine?.origin} right={<><span className="chat__remote-context">on {machine?.name ?? 'unknown machine'}</span><span className={`activity-chip${machine?.status === 'online' ? ' is-working' : ''}`} role="status">{machine ? statusLabel[machine.status] : 'Unavailable'}</span></>} />
    <ChatTranscript aria-busy={messages === null}>
      {loadError && <p className="chat-notice is-error" role="alert">{loadError} History will be retried.</p>}
      {machine && machine.status !== 'online' && <p className="chat-notice is-warn" role="status">{machine.status === 'unauthorized' ? 'Peer access was revoked. Pair this machine again in Settings > Machines.' : 'This agent is unavailable. Existing messages remain saved; reconnect the machine before sending.'}</p>}
      {!machine || !agent ? <ChatEmptyState agent={{ id: '', name: 'Remote agent' }} title="Remote agent unavailable">{!machine ? 'This machine has been unpaired.' : 'This agent is no longer in the remote directory.'}</ChatEmptyState> : messages === null ? <p className="empty-chat__sub" role="status">Loading conversation...</p> : !messages.length ? <ChatEmptyState agent={agent ?? { id: '', name: 'remote agent' }} title={`Message ${agent?.name ?? 'remote agent'}`}>Only your message is shared. The agent runs on {machine?.name ?? 'its own machine'} and keeps its local approval rules.</ChatEmptyState> : messages.map(item => <article className="turn-block" id={`turn-${item.id}`} tabIndex={-1} key={item.id}>
        <HumanMessage text={item.text} label="Your message"><time className="message-time" dateTime={new Date(item.created).toISOString()}>{new Date(item.created).toLocaleTimeString('en-US', { hour: '2-digit', minute: '2-digit' })}</time></HumanMessage>
        <AgentMessage><TurnActivity remote turn={{ status: item.state === 'uncertain' ? 'interrupted' : item.status, paused: item.status === 'waiting' ? 1 : 0, created: new Date(item.created).toISOString() }} agentName={agent?.name ?? 'remote agent'} onOpenTask={id => document.getElementById(`turn-${id}`)?.focus()} /><div className="message-body">{item.error && <p className="message-error" role="alert">{item.error}</p>}{item.reply ? <div className="legacy"><Markdown results text={item.reply} scope="" origin={window.location.origin} /></div> : !['failed', 'interrupted', 'cancelled'].includes(item.status) && <p className="message-muted" role="status">Waiting for {agent?.name}...</p>}</div><TurnStatus tone={remoteTone(item)} label={remoteStatus(item)} /></AgentMessage>
      </article>)}<div ref={end} />
    </ChatTranscript>
    <footer className="composer-area">
      {disabledReason && <p id="remote-send-reason" className="composer__note" role="status">{disabledReason}</p>}
      {!disabledReason && provider && <p className="composer__note">{provider.name}{(agent?.model || provider.defaultModel) && <> · <code>{agent?.model || provider.defaultModel}</code></>}</p>}
      {error && <div className="composer__error" role="alert">{error}</div>}
      {(invalid || pending && !busy) && <div className="composer__error" role="status"><span><strong>{invalid ? 'Saved message could not be read' : 'Message submission is not confirmed'}</strong>. Check conversation history. Retry keeps the same request ID and text.</span><Button size="sm" variant="ghost" disabled={busy} onClick={() => void discard()}>Discard saved message</Button></div>}
      {machine && agent && <ChatComposer label={`Message ${agent.name} on ${machine.name}`} input={{ id: 'remote-message', value: text, required: true, disabled: busy || !!pending || !!disabledReason, 'aria-describedby': disabledReason ? 'remote-send-reason' : undefined, onChange: event => { setText(event.target.value); try { sessionStorage.setItem(storage + ':draft', event.target.value); } catch { setError('Draft could not be saved in browser storage.'); } } }}
        onSubmit={event => { event.preventDefault(); void send(); }} onSendKey={() => void send()} attachmentDisabledReason="Remote conversations support text only."
        busy={busy} retry={!!pending} sendDisabled={busy || !!disabledReason || !text.trim()} sendLabel={busy ? 'Sending...' : pending ? 'Retry send' : 'Send message'} sendTitle={disabledReason ?? (pending ? 'Retry send' : 'Send (Enter)')} />}
    </footer>
  </main>{detailsOpen && rightPanel.result && <RightPanel path="" name={agent?.name ?? 'Remote agent'} state={rightPanel} request={request} onClose={() => setDetailsOpen(false)} details={null} activity={null} />}</>;
}

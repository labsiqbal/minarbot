import { readClipboardText } from './ui/clipboard';
import { MessageCopyButton } from './ui/MessageCopyButton';
import { useCallback, useEffect, useRef, useState } from 'react';
import { useVisiblePolling } from './hooks/useVisiblePolling';
import { SecureComputerRequest } from './SecureComputerRequest';
import { useComputerSettings } from './AgentComputerSettings';
import RFB from '@novnc/novnc';
import { ClipboardType, Ellipsis, ExternalLink, Monitor, RefreshCw, X } from 'lucide-react';
import { ActionMenu, ActionPopover, Alert, Badge, Button, Card, Field, IconButton, Input, useConfirm, RecoveryAlert } from './ui/kit';
import type { ComputerWait } from '../shared/computer';
import type { ComputerRequest, ComputerStatus } from './AgentComputerPanel';
import './computer-viewer.css';

type ControlCommand = { id: string; revision: number; operation: 'take' | 'hand-back' | 'dismiss'; turn?: string };
type ScreenError = 'limit' | 'connection' | 'timeout' | 'stopped';
const screenErrorText: Record<ScreenError, string> = {
  limit: 'Viewer limit reached. Close another viewer.',
  connection: 'Screen connection failed.',
  timeout: 'Screen connection timed out.',
  stopped: 'Desktop stopped.',
};
export function useComputerControl(agent: string, request: ComputerRequest, suspended = false) {
  const key = `minarbot-computer-control:${agent}`;
  const [status, setStatus] = useState<ComputerStatus | null>(null), [error, setError] = useState('');
  const [pending, setPending] = useState<ControlCommand | null>(null), [busy, setBusy] = useState(false), [invalid, setInvalid] = useState(false);
  const statusReads = useRef(0);
  const updateStatus = useCallback((value: ComputerStatus) => { statusReads.current++; setStatus(value); }, []);
  const mounted = useRef(true), flight = useRef(false), confirmed = useRef(false), confirm = useConfirm();
  useEffect(() => {
    mounted.current = true;
    try {
      const saved = JSON.parse(sessionStorage.getItem(key) ?? 'null');
      if (saved) {
        if (typeof saved !== 'object' || Object.keys(saved).some(k => !['id','revision','operation','turn'].includes(k)) || typeof saved.id !== 'string' || !/^[a-f0-9-]{36}$/i.test(saved.id) || !Number.isSafeInteger(saved.revision) || saved.revision < 0 || !['take','hand-back','dismiss'].includes(saved.operation) || saved.operation === 'dismiss' && typeof saved.turn !== 'string') throw new Error();
        setPending(saved);
      }
    } catch { setInvalid(true); }
    return () => { mounted.current = false; };
  }, [agent, request, key]);
  const ref = useVisiblePolling(useCallback(async current => {
    if (suspended) return;
    const read = statusReads.current;
    try { const value = await request<ComputerStatus>(`/api/agents/${agent}/computer`); if (current() && !flight.current && read === statusReads.current) { setStatus(value); } }
    catch (error) { if (current()) setError(error instanceof Error ? error.message : 'Computer status unavailable.'); }
  }, [agent, request, suspended]), 1000);
  async function control(operation: ControlCommand['operation'], turn?: string) {
    if (!status || flight.current || invalid) return;
    if (!pending && operation === 'take' && !confirmed.current) {
      if (!await confirm({ title: 'Take control?', description: 'Computer tools will pause until you hand control back. Closing this viewer keeps you in control.', confirm: 'Take control' })) return;
      confirmed.current = true;
    }
    if (flight.current || !mounted.current) return;
    const command = pending ?? { id: crypto.randomUUID(), revision: status.control.revision, operation, ...(turn ? { turn } : {}) };
    try { sessionStorage.setItem(key, JSON.stringify(command)); }
    catch { setError('Browser could not save control request. Check site storage.'); return; }
    flight.current = true; setBusy(true); setPending(command); setError('');
    try {
      const value = await request<ComputerStatus>(`/api/agents/${agent}/computer/control`, command);
      sessionStorage.removeItem(key);
      if (mounted.current) { updateStatus(value); setPending(null); }
    } catch (error) { if (mounted.current) setError(error instanceof Error ? error.message : 'Control change is not confirmed. Retry the saved request.'); }
    finally { flight.current = false; if (mounted.current) setBusy(false); }
  }
  async function discard() {
    if (!await confirm({ title: 'Load current control?', description: 'Accepted changes still apply. Check current control before taking another action.', confirm: 'Load current control' })) return;
    try {
      const value = await request<ComputerStatus>(`/api/agents/${agent}/computer`);
      sessionStorage.removeItem(key); updateStatus(value); setPending(null); setInvalid(false); setError('');
    } catch (error) { setError(error instanceof Error ? error.message : 'Computer status unavailable.'); }
  }
  const recovery = <>
    {error && <Alert tone="error">{error}</Alert>}
    {pending && <RecoveryAlert inFlight={busy} progress={false} tone="warn" title="Control change not confirmed" actions={<><Button size="sm" variant="primary" onClick={() => void control(pending.operation, pending.turn)}>Retry saved request</Button><Button size="sm" onClick={() => void discard()}>Load current control</Button></>}>Couldn't confirm the change. Retry to finish it, or load the latest status.</RecoveryAlert>}
    {invalid && <Alert tone="error" title="Saved control request could not be read" actions={<Button size="sm" onClick={() => void discard()}>Load current control</Button>}>Check the latest computer status before sending another request.</Alert>}
  </>;
  return { ref, status, setStatus: updateStatus, control, setError, busy, locked: busy || !!pending || invalid, recovery };
}

export function ComputerRequestCard({ agent, name, wait, request }: { agent: string; name: string; wait: ComputerWait; request: ComputerRequest }) {
  const controls = useComputerControl(agent, request);
  return <div ref={controls.ref} className="bs-root computer-request"><Card>
    <p><strong>{wait.secret_entry ? `${name} needs a secure value for ${wait.secret_origin}` : `${name} needs you to take over: ${wait.reason || 'The user is controlling this computer.'}`}</strong></p>
    <div className="k-inline">{controls.status?.control.mode === 'user'
      ? <Button size="sm" busy={controls.busy} disabled={controls.locked} onClick={() => void controls.control('hand-back')}>Hand back to agent</Button>
      : <>{wait.secret_entry ? <SecureComputerRequest key={wait.call_id} agent={agent} wait={wait} request={request} disabled={!controls.status || controls.locked} /> : <Button size="sm" variant="primary" busy={controls.busy} disabled={!controls.status || controls.locked} onClick={() => void controls.control('take')}>Take control</Button>}<Button size="sm" disabled={!controls.status || controls.locked} onClick={() => void controls.control('dismiss', wait.turn)}>Dismiss</Button></>}
    </div>
  </Card>{controls.recovery}</div>;
}

export function ComputerViewer({ agent, name, request, popout = false, onClose, url }: { url?: string; agent: string; name: string; request: ComputerRequest; popout?: boolean; onClose?: () => void }) {
  const [stopping, setStopping] = useState(false);
  const controls = useComputerControl(agent, request, stopping), { status } = controls;
  const settings = useComputerSettings(agent, request, controls.setStatus);
  const confirm = useConfirm();
  const [visible, setVisible] = useState(!document.hidden), [text, setText] = useState(''), [screenError, setScreenError] = useState<ScreenError | null>(null), [starting, setStarting] = useState(false), [sendingText, setSendingText] = useState(false);
  const [connected, setConnected] = useState(false), [connection, setConnection] = useState(0);
  const screen = useRef<HTMLDivElement>(null), canvas = useRef<HTMLDivElement>(null), rfb = useRef<RFB | null>(null), handBack = useRef<HTMLButtonElement>(null), inputFlight = useRef(0);
  const user = status?.control.mode === 'user', running = !!status && ['running','idle'].includes(status.state);
  useEffect(() => {
    if (!running || !visible || controls.locked || stopping || !canvas.current) return;
    let stopped = false;
    let socket: WebSocket | undefined, client: RFB | undefined;
    setConnected(false); setScreenError(null);
    const element = canvas.current;
    const connecting = new AbortController();
    const fail = (reason: ScreenError) => {
      connecting.abort();
      if (stopped) return;
      setConnected(false);
      setScreenError(reason);
    };
    AbortSignal.timeout(15000).addEventListener('abort', () => {
      fail('timeout');
      stopped = true;
      socket?.close();
    }, { signal: connecting.signal });
    const connectionFailed = () => {
      fail('connection');
      void request<ComputerStatus>(`/api/agents/${agent}/computer`).then(latest => {
        if (!stopped && latest.state === 'off') fail('stopped');
      }).catch(() => {});
    };
    void request<{ ticket: string }>(`/api/agents/${agent}/computer/vnc-ticket`, { mode: user ? 'control' : 'watch', revision: status?.control.revision }).then(({ ticket }) => {
      if (stopped) return;
      const url = new URL(`/api/agents/${agent}/computer/vnc`, location.href); url.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:'; url.searchParams.set('ticket', ticket);
      const channel = new WebSocket(url, ['minarbot-vnc-v1', 'binary']);
      socket = channel;
      channel.binaryType = 'arraybuffer';
      channel.addEventListener('close', event => {
        if (stopped) return;
        if (event.code === 4408) fail('limit');
        else if (event.code === 4409) fail('stopped');
        else if (event.code === 4410) fail('timeout');
        else connectionFailed();
      }, { once: true });
      channel.addEventListener('open', () => {
        const opened = performance.now();
        const attach = (slow: boolean) => {
          if (stopped) return;
          client = new RFB(element, channel, { shared: true });
          if (slow) { client.qualityLevel = 0; client.compressionLevel = 6; }
          client.viewOnly = !user; client.scaleViewport = true; client.resizeSession = false; client.background = 'transparent'; client.focusOnClick = !!user;
          client.addEventListener('connect', () => {
            connecting.abort();
            if (!stopped) setConnected(true);
          }, { once: true });
          client.addEventListener('clipboard', event => {
            const value = (event as CustomEvent<{ text?: unknown }>).detail?.text;
            if (stopped || !user || typeof value !== 'string') return;
            if (value.length > 4000) { controls.setError('Clipboard text exceeds 4000 characters. Copy a smaller selection.'); return; }
            setText(value);
          });
          rfb.current = client;
        };
        // The 16 KiB sample takes about 130 ms at 1 Mbps on the viewer socket.
        if (channel.protocol === 'minarbot-vnc-v1') channel.addEventListener('message', () => attach(performance.now() - opened > 130), { once: true });
        else attach(false);
      }, { once: true });
    }).catch(error => {
      if (stopped) return;
      if (error?.code === 'viewer_limit') fail('limit');
      else if (error?.code === 'desktop_stopped') fail('stopped');
      else connectionFailed();
    });
    return () => {
      stopped = true;
      connecting.abort();
      if (socket && socket.readyState < WebSocket.CLOSING) {
        if (client) client.disconnect();
        else socket.close();
      }
      rfb.current = null; element.replaceChildren();
    };
  }, [agent, running, visible, connection, user, controls.locked, stopping, status?.control.revision, request]);
  useEffect(() => {
    const change = () => setVisible(!document.hidden);
    document.addEventListener('visibilitychange', change);
    return () => document.removeEventListener('visibilitychange', change);
  }, []);
  async function input(name: string, args: unknown) {
    if (!status || !running || !user || controls.locked) return false;
    if (inputFlight.current >= 8) { controls.setError('Computer input is busy. Wait before sending more input.'); return false; }
    inputFlight.current++;
    try { await request(`/api/agents/${agent}/computer/input`, { revision: status.control.revision, name, args }); return true; }
    catch (error) { controls.setError(error instanceof Error ? error.message : 'Input outcome is not confirmed. Check the screen before sending more input.'); return false; }
    finally { inputFlight.current--; }
  }
  useEffect(() => {
    const escape = (event: globalThis.KeyboardEvent) => {
      if (!running || !user || controls.locked || !(event.target instanceof Node) || !screen.current?.contains(event.target)) return;
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'v') {
        event.preventDefault(); event.stopImmediatePropagation();
        void readClipboardText().then(value => {
          if (value.length > 4000) { controls.setError('Clipboard text exceeds 4000 characters. Copy a smaller selection.'); return; }
          if (value) void input('computer_type', { text: value });
        }).catch(error => controls.setError(error.message));
        return;
      }
      if (event.key !== 'Escape') return;
      event.preventDefault(); event.stopImmediatePropagation();
      if (event.shiftKey) { rfb.current?.blur(); handBack.current?.focus(); }
      else rfb.current?.sendKey(0xff1b, 'Escape');
    };
    window.addEventListener('keydown', escape, true);
    return () => window.removeEventListener('keydown', escape, true);
  }, [running, user, controls.locked]);
  async function start() {
    if (starting || settings.locked || controls.locked) return; setStarting(true); controls.setError('');
    try {
      if (!status?.enabled) {
        const enabled = await settings.save(true);
        if (!enabled?.enabled) return;
        controls.setStatus(enabled);
      }
      controls.setStatus(await request<ComputerStatus>(`/api/agents/${agent}/computer/start`, {}));
    }
    catch (error) { controls.setError(error instanceof Error ? error.message : 'Computer did not start.'); }
    finally { setStarting(false); }
  }
  async function stopComputer() {
    if (!status || controls.locked || stopping) return;
    if (!await confirm({ title: 'Stop computer?', description: 'The desktop will close and any unsaved work inside it may be lost.', confirm: 'Stop computer', danger: true })) return;
    setStopping(true);
    try { controls.setStatus(await request<ComputerStatus>(`/api/agents/${agent}/computer/stop`, {})); }
    catch (error) { controls.setError(error instanceof Error ? error.message : 'Computer did not stop.'); }
    finally { setStopping(false); }
  }
  const statusLabel = !status ? 'Loading' : !status.available ? 'Unavailable' : !status.enabled ? 'Disabled' : starting || status.state === 'starting' ? 'Starting' : running ? 'Live' : 'Off';
  const statusTone = statusLabel === 'Live' ? 'success' : statusLabel === 'Starting' ? 'warn' : 'neutral';
  const waits = running ? status?.requests ?? [] : [];
  const locked = controls.locked || settings.locked || starting || stopping;
  const placeholder = !visible ? 'Viewer paused while window is hidden.' : screenError ? screenErrorText[screenError] : !status ? 'Loading computer status...' : !status.available ? 'Computer is disabled on this server.' : !status.enabled ? 'No computer assigned.' : starting || status.state === 'starting' ? 'Starting computer...' : running ? 'Connecting to computer...' : 'Computer is off.';
  return <div ref={controls.ref} className="computer-viewer bs-root">
    {url && <div className="k-row-actions"><span className="right-panel__path">{url}</span><Button size="sm" disabled={!running || !user || controls.locked} onClick={() => void input('computer_open_url', { url })}>Open URL</Button></div>}
    <div ref={settings.section} className="computer-viewer__poll" />
    <header className="computer-viewer__toolbar" hidden={!running && !onClose}>
      <div className="computer-viewer__identity">
        <div className="computer-viewer__heading"><h2 className="computer-viewer__title" title={`${name}'s computer`}>{name}'s computer</h2>
          <Badge tone={statusTone}><span className={`computer-viewer__status-dot ${statusTone === 'success' ? 'is-live' : statusTone === 'warn' ? 'is-starting' : ''}`} aria-hidden />{statusLabel}</Badge>
        </div>
        {running && <div className="computer-viewer__meta"><span className="computer-viewer__owner">{user ? "You're in control" : 'Agent in control'}</span>
          {status?.stopsInMs != null && <span className="computer-viewer__since">Stops in {Math.max(1, Math.ceil(status.stopsInMs / 60000))} min</span>}
        </div>}
      </div>
      <div className="computer-viewer__actions">
        {running && !popout && <IconButton label="Pop out" onClick={() => window.open(`/?computer=${encodeURIComponent(agent)}`, '_blank', 'popup,width=1280,height=960,noopener')}><ExternalLink size={14} aria-hidden /></IconButton>}
        {running && user && <ActionPopover label="Paste text into computer" trigger={<IconButton label="Paste text" disabled={locked}><ClipboardType size={14} aria-hidden /></IconButton>}>
          <form className="computer-viewer__text" onSubmit={async event => { event.preventDefault(); if (!text || sendingText || !running || locked) return; setSendingText(true); const sent = text; try { const accepted = await input('computer_type', { text: sent }); if (accepted) setText(current => current === sent ? '' : current); } finally { setSendingText(false); } }}>
            <Field label="Type or paste text"><Input value={text} maxLength={4000} onChange={event => setText(event.target.value)} disabled={locked} /></Field>
            <div className="k-row-actions"><Button size="sm" disabled={!connected || locked} onClick={() => rfb.current?.clipboardPasteFrom(text)}>Set clipboard</Button><MessageCopyButton text={text} label="Copy computer clipboard" /><Button size="sm" variant="primary" type="submit" busy={sendingText} disabled={!text || locked}>Send text</Button></div>
          </form>
        </ActionPopover>}

        {running && (user ? <Button ref={handBack} size="sm" variant="primary" busy={controls.busy} disabled={locked} onClick={() => void controls.control('hand-back')}>Hand back</Button> : <Button size="sm" variant="primary" busy={controls.busy} disabled={!status?.enabled || !status.available || locked} onClick={() => void controls.control('take')}>Take control</Button>)}
        {running && <ActionMenu label="More computer actions" trigger={<IconButton label="More computer actions" disabled={stopping} aria-busy={stopping || undefined}>{stopping ? <span className="k-spinner" aria-hidden /> : <Ellipsis size={16} aria-hidden />}</IconButton>} items={[
          { label: 'Reconnect', icon: <RefreshCw size={14} aria-hidden />, disabled: locked, onSelect: () => setConnection(value => value + 1) },
          'separator', { label: 'Stop computer', danger: true, icon: <Monitor size={14} aria-hidden />, disabled: locked, onSelect: () => void stopComputer() },
        ]} />}
        {onClose && <IconButton label="Close" onClick={onClose}><X size={16} aria-hidden /></IconButton>}
      </div>
    </header>
    {!!waits.length && <div className="computer-viewer__requests">{waits.map(wait => <div className="computer-viewer__request" key={wait.turn}><span>{wait.secret_entry ? `${name} needs a secure value for ${wait.secret_origin}` : `${name} needs you to take over: ${wait.reason || 'Computer access is waiting for you.'}`}</span>{wait.secret_entry && !user && <SecureComputerRequest key={wait.call_id} agent={agent} wait={wait} request={request} disabled={locked} />}{!user && <Button size="sm" variant="ghost" disabled={locked} onClick={() => void controls.control('dismiss', wait.turn)}>Dismiss</Button>}</div>)}</div>}
    {controls.recovery}{settings.recovery}
    {!running && user && <p className="computer-viewer__idle-control">You still hold control. <Button size="sm" variant="ghost" busy={controls.busy} disabled={locked} onClick={() => void controls.control('hand-back')}>Hand back</Button></p>}
    <div ref={screen} className={`computer-viewer__screen${running && user ? ' is-controlled' : ''}`} title={running && user ? 'Shift+Escape returns to controls' : undefined} role={running ? 'application' : undefined} aria-label={running ? `${name}'s computer. Click to send keys. Shift+Escape returns to controls.` : undefined}>
      {running && <div ref={canvas} className="computer-viewer__canvas" aria-label={`${name}'s live computer screen`} />}
      {(!running || !visible || !connected) && <div className="computer-viewer__empty" role="status" aria-live="polite">
        {(starting || status?.state === 'starting' || running && visible && !screenError) ? <span className="k-spinner computer-viewer__spinner" aria-hidden /> : <Monitor size={28} strokeWidth={1.5} aria-hidden />}
        {(running || starting || status?.state === 'starting' || !status || screenError === 'stopped') && <p>{placeholder}</p>}
        {screenError && running && visible ? <Button size="sm" onClick={() => setConnection(value => value + 1)}>Reconnect</Button> : !running && status?.available && !starting && status.state !== 'starting' && <Button size="sm" variant="primary" disabled={locked} onClick={() => void start()}>Turn on computer</Button>}
      </div>}
    </div>
  </div>;
}

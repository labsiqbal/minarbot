import { useCallback, useEffect, useRef, useState } from 'react';
import { Alert, Button, Card, Progress, Row, SkeletonRows, Switch, useConfirm, RecoveryAlert } from './ui/kit';
import { useVisiblePolling } from './hooks/useVisiblePolling';
import type { ComputerRequest, ComputerStatus } from './AgentComputerPanel';
import { computerSetupLabel } from './ComputerSetup';
import { parseRoute, routePath } from './navigation';
type Command = { id: string; revision: number; enabled: boolean };
export function useComputerSettings(agent: string, request: ComputerRequest, onSaved?: (status: ComputerStatus) => void) {
  const key = `minarbot-computer:${agent}`;
  const [status, setStatus] = useState<ComputerStatus | null>(null), [pending, setPending] = useState<Command | null>(null), [error, setError] = useState(''), [busy, setBusy] = useState(false), [invalid, setInvalid] = useState(false);

  const mounted = useRef(true), flight = useRef(false), confirm = useConfirm();
  useEffect(() => {
    mounted.current = true;
    try {
      const raw = sessionStorage.getItem(key);
      if (raw) { const saved = JSON.parse(raw); if (!saved || typeof saved !== 'object' || Object.keys(saved).length !== 3 || typeof saved.id !== 'string' || !/^[a-f0-9-]{36}$/i.test(saved.id) || !Number.isSafeInteger(saved.revision) || saved.revision < 0 || typeof saved.enabled !== 'boolean') throw new Error(); setPending(saved); }
    } catch { setInvalid(true); }
    return () => { mounted.current = false; };
  }, [agent, key, request]);
  const section = useVisiblePolling(useCallback(async current => {
    if (flight.current) return;
    try { const next = await request<ComputerStatus>(`/api/agents/${agent}/computer`); if (current() && !flight.current) { setStatus(next); if (!pending && !invalid) setError(''); } }
    catch (error) { if (current()) setError(error instanceof Error ? error.message : 'Computer status unavailable.'); }
  }, [agent, request, pending, invalid]));
  async function save(enabled: boolean) {
    if (flight.current || !status || invalid) return;
    const command = pending ?? { id: crypto.randomUUID(), revision: status.revision, enabled };
    try { sessionStorage.setItem(key, JSON.stringify(command)); } catch { setError('Browser could not save recovery data. Check site storage.'); return; }
    flight.current = true; setPending(command); setBusy(true); setError('');
    try { const next = await request<ComputerStatus>(`/api/agents/${agent}/computer`, command); if (!mounted.current) return; sessionStorage.removeItem(key); setStatus(next); setPending(null); onSaved?.(next); return next; }
    catch (error) { if (mounted.current) setError(error instanceof Error ? error.message : 'Computer change is not confirmed. Retry same request.'); }
    finally { flight.current = false; if (mounted.current) setBusy(false); }
  }
  async function reload() {
    if (flight.current) return;
    if ((pending || invalid) && !await confirm({ title: 'Discard computer request?', description: 'Changes already accepted by the server still apply.', confirm: 'Discard and reload', danger: true })) return;
    flight.current = true; setBusy(true);
    try { const next = await request<ComputerStatus>(`/api/agents/${agent}/computer`); if (!mounted.current) return; sessionStorage.removeItem(key); setStatus(next); setPending(null); setInvalid(false); setError(''); }
    catch (error) { if (mounted.current) setError(error instanceof Error ? error.message : 'Status could not load.'); }
    finally { flight.current = false; if (mounted.current) setBusy(false); }
  }
  return { section, status, target: pending?.enabled ?? status?.enabled ?? false, action: pending?.enabled ? 'Turning on computer...' : 'Stopping computer...', save, busy, locked: busy || !status || !!pending || invalid, recovery: <>    {pending && <RecoveryAlert inFlight={busy} progress={false} tone="warn" title="Couldn't confirm the change" actions={<><Button size="sm" variant="primary" onClick={() => void save(pending.enabled)}>Try again</Button><Button size="sm" onClick={() => void reload()}>Reload</Button></>} />}
    {invalid && <Alert tone="error" actions={<Button size="sm" disabled={busy} onClick={() => void reload()}>Reload</Button>}>Saved computer request is invalid. Load latest status before continuing.</Alert>}
    {error && !pending && !invalid && <Alert tone="error" actions={<Button size="sm" disabled={busy} onClick={() => void reload()}>Reload</Button>}>{error}</Alert>}</> };
}

export function AgentComputerSettings({ agent, request, autoFocus = false }: { agent: string; request: ComputerRequest; autoFocus?: boolean }) {
  const settings = useComputerSettings(agent, request);
  const { section, status, target, action, busy, locked, save, recovery } = settings;
  useEffect(() => { if (!autoFocus) return; const frame = requestAnimationFrame(() => { section.current?.scrollIntoView({ block: 'center' }); section.current?.querySelector<HTMLElement>(status?.available ? '[role="switch"]' : 'a[href]')?.focus(); }); return () => cancelAnimationFrame(frame); }, [autoFocus, section, status?.available]);
  const setupPath = routePath({ ...parseRoute(location.pathname + location.search), settings: 'computer' });
  return <div ref={section} className="bs-root k-stack">
    {!status ? <><span className="sr-only" role="status">Loading computer status...</span><SkeletonRows count={1} /></> : <Card>
      <Row title="Computer" meta={busy ? <Progress>{action}</Progress> : !status.available && <>
        <span>{status.setup ? computerSetupLabel(status.setup) : 'Setup required'}</span><a className="k-link" href={setupPath}>Computer setup</a>
      </>} trailing={<Switch label="Computer" checked={target} disabled={locked || !status.available} onChange={value => void save(value)} />} />
    </Card>}
    {recovery}
  </div>;
}

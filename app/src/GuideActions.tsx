import { useId, useRef, useState } from 'react';
import type { Agent, FirstRunConnection } from './AgentProfileDialog';
import { useFirstRunConnections } from './FirstRun';
import { Alert, Badge, Button, Dialog, Field, Select } from './ui/kit';
import { parseRoute, routePath } from './navigation';
import screens from './guide-screens.json';
import './guide.css';

type Request = <T>(path: string, body?: unknown) => Promise<T>;

const titles: Record<string, string> = {
  create_agent: 'Create agent',
  update_agent: 'Update agent',
  create_routine: 'Create routine',
  update_routine: 'Update routine',
  pause_routine: 'Pause routine',
  resume_routine: 'Resume routine',
  connect_app: 'Connect app',
  set_connected_app_access: 'Change app access',
};
const labels: Record<string, string> = {
  agent: 'Agent ID', id: 'Routine ID', currentName: 'Current name', providerAccount: 'Provider', reportTo: 'Report to',
  toolBudget: 'Tool steps per task', backupModel: 'Fallback model', confirmRuntime: 'Switch runtime',
};
const statusLabels: Record<string, string> = {
  pending: 'Ready for review', applied: 'Applied', applying: 'Applying…', cancelled: 'Cancelled',
  expired: 'Expired', failed: 'Failed', unknown: 'Check settings to confirm',
};
type Change = {
  id: string;
  status: string;
  change: { tool: string; target: Record<string, unknown> | null; settings: Record<string, unknown> };
};
export type GuideActions = { changes: Change[]; screens: string[] };

function fieldValue(value: unknown) {
  if (value === null || value === '') return 'Default';
  if (typeof value === 'boolean') return value ? 'Yes' : 'No';
  return typeof value === 'string' ? value : JSON.stringify(value, null, 2);
}

function GuideChange({ proposal, request, onChanged, disabled }: { proposal: Change; request: Request; onChanged: () => void; disabled: boolean }) {
  const heading = useRef<HTMLHeadingElement>(null), label = useId(), flight = useRef(false);
  const [status, setStatus] = useState(''), [error, setError] = useState(''), [link, setLink] = useState('');
  const current = status || proposal.status;
  const title = titles[proposal.change.tool] ?? 'Proposed change';
  const fields = [
    ...Object.entries(proposal.change.target ?? {}).map(([key, value]) => [key === 'name' ? 'currentName' : key, value] as const),
    ...Object.entries(proposal.change.settings),
  ];
  async function review(action: 'apply' | 'cancel') {
    if (flight.current || current !== 'pending' || disabled) return;
    flight.current = true;
    setError(''); setStatus('applying');
    try {
      const result = await request<{ status: string; redirect_url?: string }>(`/api/guide/changes/${proposal.id}`, { action, change: proposal.change });
      setStatus(result.status);
      setLink(result.redirect_url ?? '');
      heading.current?.focus();
    } catch (error) {
      setStatus('');
      setError(error instanceof Error ? error.message : 'Check settings before applying again.');
    } finally {
      flight.current = false;
      onChanged();
    }
  }
  return <article className="k-card guide-change" aria-labelledby={label}>
    <header className="guide-change__header">
      <h3 ref={heading} tabIndex={-1} id={label}>{title}</h3>
      <span role="status"><Badge tone={current === 'pending' ? 'warn' : 'neutral'}>{statusLabels[current] ?? current}</Badge></span>
    </header>
    <dl className="guide-change__fields">{fields.map(([key, value]) => <div key={key}>
      <dt>{labels[key] ?? key.charAt(0).toUpperCase() + key.slice(1)}</dt><dd>{key === 'runtime' && value === 'api' ? 'API' : key === 'backupModel' && value === '' ? 'Clear fallback model' : fieldValue(value)}</dd>
    </div>)}</dl>
    {error && <Alert tone="error">{error}</Alert>}
    {link && <a className="k-link" href={link} target="_blank" rel="noopener noreferrer">Finish app sign-in</a>}
    {current === 'pending' && <footer className="k-row-actions">
      <Button disabled={disabled} onClick={() => void review('cancel')}>Cancel</Button>
      <Button variant="primary" disabled={disabled} onClick={() => void review('apply')}>Apply</Button>
    </footer>}
  </article>;
}

export function GuideTurnActions({ actions, request, onChanged, disabled }: { actions: GuideActions; request: Request; onChanged: () => void; disabled: boolean }) {
  return <>
    {actions.changes.map(change => <GuideChange key={change.id} proposal={change} request={request} onChanged={onChanged} disabled={disabled} />)}
    {!!actions.screens.length && <div className="k-row-actions guide-screen-links">{actions.screens.map(id => {
      if (!Object.hasOwn(screens, id)) return null;
      const screen = screens[id as keyof typeof screens], route = parseRoute(screen.path);
      if (route.notFound) return null;
      const current = parseRoute(window.location.pathname + window.location.search);
      const href = route.settings ? routePath({ ...current, settings: route.settings }) : screen.path;
      return <a key={id} className="k-link" href={href}>Open {screen.title}</a>;
    })}</div>}
  </>;
}

export function saveGuideConnection(request: Request, agent: Agent, connection: FirstRunConnection) {
  const { runtime, account, model } = connection.defaults;
  return request<Agent>(`/api/agents/${agent.id}/edit`, { id: crypto.randomUUID(), revision: agent.revision, runtime, providerAccount: account, model, confirmRuntime: true });
}

export function GuideConnectionDialog({ agent, request, onSaved, onClose, onProviders, onEngines }: { agent: Agent; request: Request; onSaved: () => Promise<void>; onClose: () => void; onProviders: () => void; onEngines: () => void }) {
  const { selection, connections, setSelectedValue, accounts, engines, logins, error } = useFirstRunConnections(request, false, agent);
  const pending = accounts.busy || engines.busy || logins.busy;
  const [busy, setBusy] = useState(false), [saveError, setSaveError] = useState('');
  const form = useId();
  return <Dialog open title={`AI connection for ${agent.name}`} onOpenChange={open => { if (!open && !busy) onClose(); }}
    footer={<Button form={form} variant="primary" type="submit" busy={busy} disabled={!selection || busy}>Use connection</Button>}>
    <form className="k-stack" id={form} onSubmit={async event => {
      event.preventDefault();
      if (!selection || busy) return;
      setBusy(true); setSaveError('');
      try { await saveGuideConnection(request, agent, selection); await onSaved(); }
      catch (error) { setSaveError(error instanceof Error ? error.message : 'Could not save the AI connection.'); }
      finally { setBusy(false); }
    }}>
      {selection ? <Field label="Run with"><Select value={selection.value} onChange={setSelectedValue} disabled={busy} options={connections.map(item => ({ value: item.value, label: item.label }))} /></Field>
        : <p role="status">{pending ? 'Checking AI connections…' : 'No AI connection available.'}</p>}
      {error && <Alert tone="error" actions={<Button onClick={() => { accounts.reload(); engines.reload(); logins.reload(); }}>Retry</Button>}>Could not load all AI connections.</Alert>}
      {saveError && <Alert tone="error">{saveError}</Alert>}
      <div className="k-row-actions"><Button onClick={onProviders}>Providers</Button><Button onClick={onEngines}>Engines</Button></div>
    </form>
  </Dialog>;
}

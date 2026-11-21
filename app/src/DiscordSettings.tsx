import { useId, useRef, useState } from 'react';
import type { LocalProject } from '../shared/local-project';
import { usePolling } from './hooks/usePolling';
import { Alert, Badge, Button, Card, Field, Input, Row, Section, Select, SkeletonRows, useConfirm } from './ui/kit';

type DiscordRoute = { id: string; channel: string; kind: 'agent' | 'section'; target: string; users: string[]; label?: string };
type Status = {
  generation: string;
  bot: string | null;
  status: keyof typeof statusLabels;
  routes: DiscordRoute[];
  deliveries: { id: string; channel: string | null; status: 'failed' | 'uncertain' | 'revoked'; part: number; total: number; error: string }[];
};
const path = '/api/connected-apps/discord';
const statusLabels = {
  disconnected: 'Disconnected', connecting: 'Connecting', reconnecting: 'Reconnecting',
  connected: 'Connected', invalid_token: 'Token rejected', intents_required: 'Enable Message Content Intent',
  gateway_rejected: 'Gateway rejected connection', token_unavailable: 'Saved token unavailable',
};

export function DiscordSettings({ agents, request }: {
  agents: { id: string; name: string }[];
  request: <T>(path: string, data?: unknown) => Promise<T>;
}) {
  const title = useId(), routeTitle = useId(), confirm = useConfirm();
  const busyRef = useRef(false), fetchVersion = useRef(0);
  const [status, setStatus] = useState<Status | null>(null);
  const [projects, setProjects] = useState<LocalProject[]>([]);
  const [busy, setBusy] = useState(false), [replacing, setReplacing] = useState(false), [adding, setAdding] = useState(false);
  const [error, setError] = useState(''), [loadError, setLoadError] = useState('');
  const [channel, setChannel] = useState(''), [users, setUsers] = useState(''), [destination, setDestination] = useState('');
  usePolling(async signal => {
    if (busyRef.current) return;
    const version = fetchVersion.current;
    try {
      const next = await request<Status>(path);
      if (!signal.aborted && version === fetchVersion.current) { setStatus(next); setLoadError(''); }
    } catch (failure) {
      if (!signal.aborted && version === fetchVersion.current) setLoadError(failure instanceof Error ? failure.message : 'Discord settings unavailable.');
    }
  }, { interval: 2000 }, [request]);
  async function change(action: string, body?: object) {
    if (!status || busyRef.current) return false;
    busyRef.current = true; fetchVersion.current++; setBusy(true); setError('');
    try {
      setStatus(await request<Status>(`${path}/${action}`, { id: crypto.randomUUID(), generation: status.generation, ...body }));
      setLoadError('');
      return true;
    } catch (failure) {
      setError(action === 'connect' ? 'Discord could not connect. Check the bot token and try again.'
        : failure instanceof Error ? failure.message : 'Discord settings could not be saved.');
      return false;
    } finally { busyRef.current = false; setBusy(false); }
  }
  async function add() {
    setAdding(true); setError('');
    try {
      const result = await request<{ projects: LocalProject[] } | LocalProject[]>('/api/projects');
      setProjects(Array.isArray(result) ? result : result.projects);
    } catch {
      setError('Project sections could not load.');
    }
  }
  const choices = [
    ...agents.map(a => ({ value: `agent:${a.id}`, label: a.name })),
    ...projects.filter(p => !p.archived).flatMap(p => p.sections.filter(s => !s.archived).map(s => ({ value: `section:${s.id}`, label: `${p.name} / ${s.name}` }))),
  ];
  return <Section title="Discord" id={title}>
    {(error || loadError) && <Alert tone="error">{error || loadError}</Alert>}
    {!status && !loadError && <SkeletonRows count={1} />}
    {status && <Card><Row title="Bot connection" meta={<span role="status"><Badge tone={status.status === 'connected' ? 'success' : ['disconnected', 'connecting', 'reconnecting'].includes(status.status) ? 'neutral' : 'warn'}>{statusLabels[status.status]}</Badge></span>}
      trailing={status.bot ? <Button size="sm" disabled={busy} onClick={async () => {
        if (await confirm({ title: 'Disconnect Discord?', description: 'Discord routes will be removed.', confirm: 'Disconnect', danger: true })) {
          if (await change('disconnect')) { setAdding(false); setReplacing(false); }
        }
      }}>Disconnect</Button> : undefined} /></Card>}
    {status && (!status.bot || replacing) && <form className="k-fieldset" aria-labelledby={title} onSubmit={async event => {
      event.preventDefault();
      const form = event.currentTarget;
      const token = new FormData(form).get('discordToken');
      form.reset();
      if (typeof token === 'string' && await change('connect', { token })) setReplacing(false);
    }}>
      <Field label="Bot token"><Input type="password" name="discordToken" required autoComplete="off" spellCheck={false} maxLength={256} disabled={busy} /></Field>
      <div className="k-inline"><Button type="submit" variant="primary" busy={busy}>{status.bot ? 'Save token' : 'Connect Discord'}</Button>{replacing && <Button disabled={busy} onClick={() => setReplacing(false)}>Cancel</Button>}</div>
    </form>}
    {status?.bot && !replacing && <div><Button size="sm" disabled={busy} onClick={() => setReplacing(true)}>Replace bot token</Button></div>}
    {status?.bot && <Section title="Discord routes" id={routeTitle} actions={<Button size="sm" disabled={busy || adding} onClick={() => void add()}>Add route</Button>}>
      {!!status.routes.length && <Card>{status.routes.map(route => <Row key={route.id} title={route.label ?? choices.find(o => o.value === `${route.kind}:${route.target}`)?.label ?? (route.kind === 'section' ? 'Project section' : 'Agent')}
        meta={`${route.channel} · ${route.users.length} allowed ${route.users.length === 1 ? 'user' : 'users'}`}
        trailing={<Button size="sm" aria-label={`Remove route ${route.channel}`} disabled={busy} onClick={() => void change('remove-route', { route: route.id })}>Remove</Button>} />)}</Card>}
      {adding && <form className="k-fieldset" aria-labelledby={routeTitle} onSubmit={async event => {
        event.preventDefault();
        if (!destination) return;
        const [kind, target] = destination.split(':');
        if (await change('add-route', { channel: channel.trim(), users: users.trim().split(/[\s,]+/).filter(Boolean), kind, target })) {
          setChannel(''); setUsers(''); setDestination(''); setAdding(false);
        }
      }}>
        <Field label="Discord channel, thread, or DM ID"><Input required inputMode="numeric" pattern="[1-9][0-9]{0,19}" maxLength={20} value={channel} disabled={busy} onChange={e => setChannel(e.target.value)} /></Field>
        <Field label="Allowed Discord user IDs" hint="Separate IDs with commas."><Input required maxLength={2200} value={users} disabled={busy} onChange={e => setUsers(e.target.value)} /></Field>
        <Field label="Destination"><Select value={destination} disabled={busy} onChange={setDestination} options={[{ value: '', label: 'Choose agent or section' }, ...choices]} /></Field>
        <div className="k-inline"><Button type="submit" variant="primary" busy={busy} disabled={!destination}>Save route</Button><Button disabled={busy} onClick={() => setAdding(false)}>Cancel</Button></div>
      </form>}
    </Section>}
    {!!status?.deliveries.length && <Section title="Discord delivery issues"><Card>{status.deliveries.map(delivery => <Row key={delivery.id}
      title={delivery.status === 'uncertain' ? 'Send unconfirmed' : delivery.status === 'revoked' ? 'Send stopped' : 'Send failed'}
      meta={<>{delivery.channel && `Channel ${delivery.channel} · `}{delivery.part} of {delivery.total} parts confirmed. {delivery.error}</>} />)}</Card></Section>}
  </Section>;
}

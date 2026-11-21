import { useRef, useState } from 'react';
import { MessageCircle } from 'lucide-react';
import { usePolling } from './hooks/usePolling';
import { Alert, Badge, Button, Card, Checkbox, Dialog, EmptyState, Field, Row, Section, Select, useConfirm } from './ui/kit';
import './whatsapp-settings.css';

type Target = { id: string; name: string };
type Chat = { chat: string; label: string; kind: 'agents' | 'channels' | null; target: string | null; transcribeProvider: string | null };
type Status = {
  state: 'disconnected' | 'pairing' | 'linked' | 'needs_relink';
  connected: boolean;
  qr: string | null;
  chats: Chat[];
  deliveryProblems: { chat: string; status: string; error: string }[];
  transcription: { id: string; origin: string } | null;
};
type Request = <T>(path: string, data?: unknown) => Promise<T>;

function Mapping({ chat, targets, provider, busy, save }: {
  chat: Chat;
  targets: { value: string; label: string }[];
  provider: Status['transcription'];
  busy: boolean;
  save: (data: { kind: string | null; target: string | null; transcribe: boolean }) => void;
}) {
  const initial = chat.kind && chat.target ? `${chat.kind}/${chat.target}` : '';
  const initialVoice = !!provider && chat.transcribeProvider === provider.id;
  const [target, setTarget] = useState(initial);
  const [voice, setVoice] = useState(initialVoice);
  const [kind, id] = target.split('/');
  const dirty = target !== initial || (target && voice !== initialVoice);
  const options = [{ value: '', label: 'Unmapped' }, ...targets];
  if (initial && !targets.some(item => item.value === initial)) options.push({ value: initial, label: 'Destination unavailable' });
  return <form className="wa-settings__mapping" onSubmit={event => {
    event.preventDefault();
    if (dirty && !busy) save({ kind: kind || null, target: id || null, transcribe: !!target && voice });
  }}>
    <Field label={chat.label}><Select label={`Destination for ${chat.label}`} value={target} onChange={setTarget} options={options} disabled={busy} /></Field>
    {provider && target && <Checkbox checked={voice} onChange={setVoice} label="Transcribe voice notes" description={`Send audio to ${provider.origin}`} disabled={busy} />}
    <Button size="sm" type="submit" disabled={!dirty || busy}>Save mapping</Button>
  </form>;
}

export function WhatsAppSettings({ request, agents, sections }: { request: Request; agents: Target[]; sections: Target[] }) {
  const confirm = useConfirm();
  const [open, setOpen] = useState(false);
  const [status, setStatus] = useState<Status | null>(null);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState('');
  const [loadFailed, setLoadFailed] = useState(false);
  const version = useRef(0);
  const opener = useRef<HTMLElement | null>(null);
  const sending = useRef(false);

  usePolling(async signal => {
    const before = version.current;
    try {
      const response = await request<Status>('/api/whatsapp');
      if (!signal.aborted && before === version.current && !sending.current) {
        setStatus(response);
        setLoadFailed(false);
      }
    } catch {
      if (!signal.aborted && before === version.current && !sending.current) setLoadFailed(true);
    }
  }, { interval: open ? 1500 : 15000 }, [open, request]);

  async function change(action: string, data: Record<string, unknown> = {}) {
    if (sending.current) return;
    sending.current = true;
    setBusy(true);
    setProblem('');
    version.current++;
    try {
      setStatus(await request<Status>(`/api/whatsapp/${action}`, { id: crypto.randomUUID(), ...data }));
    } catch (error) {
      setProblem(error instanceof Error ? error.message : 'WhatsApp change could not be saved.');
    } finally {
      version.current++;
      sending.current = false;
      setBusy(false);
    }
  }

  async function unlink() {
    if (await confirm({ title: 'Unlink WhatsApp?', description: 'The link session and chat mappings will be deleted.', confirm: 'Unlink', danger: true })) await change('unlink');
  }

  const title = !status ? 'Loading' : ({
    disconnected: 'Disconnected', pairing: 'Waiting for scan', linked: status.connected ? 'Linked' : 'Linked · Reconnecting', needs_relink: 'Needs re-link',
  })[status.state];
  const tone = status?.state === 'linked' && status.connected ? 'success' : status?.state === 'needs_relink' ? 'warn' : 'neutral';
  const targets = [
    ...agents.map(item => ({ value: `agents/${item.id}`, label: item.name })),
    ...sections.map(item => ({ value: `channels/${item.id}`, label: item.name })),
  ];

  return <>
    <Card><Row title="WhatsApp" leading={<MessageCircle size={16} aria-hidden />} meta={status && <Badge tone={tone}>{title}</Badge>}
      onClick={() => { opener.current = document.activeElement instanceof HTMLElement ? document.activeElement : null; setOpen(true); }} /></Card>
    <Dialog open={open} onOpenChange={setOpen} title="WhatsApp" onCloseAutoFocus={event => {
      if (opener.current?.isConnected) { event.preventDefault(); opener.current.focus(); }
    }}>
      <div className="k-stack">
        <div className="k-inline" role="status"><Badge tone={tone}>{title}</Badge></div>
        {(problem || loadFailed) && <Alert tone="error">{problem || 'WhatsApp status is unavailable.'}</Alert>}
        {status && (status.state === 'disconnected' || status.state === 'needs_relink') && <>
          <Alert tone="warn">Unofficial WhatsApp Web link. Protocol updates may break it, and WhatsApp can ban your number.</Alert>
          <div className="k-inline">
            <Button variant="primary" busy={busy} onClick={() => void change('link')}>{status.state === 'needs_relink' ? 'Link again' : 'Link WhatsApp'}</Button>
            {status.state === 'needs_relink' && <Button disabled={busy} onClick={() => void unlink()}>Unlink WhatsApp</Button>}
          </div>
        </>}
        {status?.state === 'pairing' && <>
          {status.qr ? <div className="wa-settings__pair">
            <img src={status.qr} width={258} height={258} alt="WhatsApp linking QR code" />
            <p className="k-field__hint">On your phone, open WhatsApp → Linked devices → Link a device.</p>
          </div> : <p className="k-field__hint">Getting QR code…</p>}
          <div><Button busy={busy} onClick={() => void change('unlink')}>Cancel linking</Button></div>
        </>}
        {status?.state === 'linked' && <>
          <Section title="Chat mappings">
            {status.chats.length ? <Card>{status.chats.map(chat =>
              <Mapping key={JSON.stringify([chat.chat, chat.kind, chat.target, chat.transcribeProvider])} chat={chat} targets={targets} provider={status.transcription} busy={busy}
                save={data => void change('map', { chat: chat.chat, ...data })} />)}</Card>
              : <EmptyState title="No chats seen yet">Receive a WhatsApp message to see its chat here.</EmptyState>}
          </Section>
          {status.deliveryProblems.length > 0 && <Section title="Delivery problems"><Card>{status.deliveryProblems.map((item, index) =>
            <Row key={`${item.chat}/${index}`} title={status.chats.find(chat => chat.chat === item.chat)?.label ?? item.chat}
              meta={item.error} trailing={<Badge tone="warn">{item.status === 'uncertain' ? 'Unconfirmed' : 'Failed'}</Badge>} />)}</Card></Section>}
          <div><Button busy={busy} onClick={() => void unlink()}>Unlink WhatsApp</Button></div>
        </>}
      </div>
    </Dialog>
  </>;
}

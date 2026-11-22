import { useCachedData, useQueryVersion } from './hooks/useCachedQuery';
import { queryCache } from './hooks/queryCache';
import { useCallback, useRef, useState } from 'react';
import { Smartphone } from 'lucide-react';
import { useVisiblePolling } from './hooks/useVisiblePolling';
import { useExpiry } from './hooks/useExpiry';
import { Alert, Button, Card, Dialog, EmptyState, PageHeader, Row, Section, SkeletonRows, Switch, useConfirm } from './ui/kit';
import './devices.css';

type Device = { id: string; name: string; lastSeen: number };
type DeviceSettings = { required: boolean; canRequire: boolean; current: string | null; pairingUrl: string | null; devices: Device[] };
type PairingCode = { code: string; expiresAt: number; url: string; qr: string };
type Request = <T>(path: string, data?: unknown) => Promise<T>;

const path = '/api/device-pairing';
const signIn = 'Remote sign-in required';
const lastSeen = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' });
const message = (error: unknown) => error instanceof Error ? error.message : 'Connection failed. Try again.';

export function DevicesPanel({ request }: { request: Request }) {
  const { data: settings, fresh } = useCachedData<DeviceSettings>(path);
  const version = useQueryVersion();
  const [code, setCode] = useState<PairingCode>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [loadError, setLoadError] = useState('');
  const generation = useRef(0);
  const confirm = useConfirm();

  const load = useCallback(async (current: () => boolean) => {
    const currentGeneration = generation.current;
    try {
      await queryCache.refresh(path, () => request<DeviceSettings>(path));
      if (current() && currentGeneration === generation.current) {
        setLoadError('');
      }
    } catch (error) {
      if (current() && currentGeneration === generation.current) setLoadError(message(error));
    }
  }, [request, version]);
  const panel = useVisiblePolling(load, 5000);

  const expired = useExpiry(code?.expiresAt);

  async function update(action: () => Promise<void>) {
    generation.current++;
    setBusy(true);
    setError('');
    try { await action(); }
    catch (error) { setError(message(error)); }
    finally { generation.current++; setBusy(false); }
  }

  function issue() {
    void update(async () => {
      const started = Date.now();
      const origin = new URLSearchParams(window.location.search).get('pairingOrigin');
      const value = await request<PairingCode>(path + '/code', origin ? { origin } : {});
      setCode({ ...value, expiresAt: started + 120_000 });
    });
  }

  function toggle(required: boolean) {
    void update(async () => {
      await request(path, { required });
    });
  }

  async function revoke(device: Device) {
    if (!await confirm({ title: 'Revoke device?', description: device.name, confirm: 'Revoke', danger: true })) return;
    await update(async () => {
      await request(path + '/revoke', { id: device.id });
      if (settings?.current === device.id) { queryCache.clear(); window.location.reload(); }
    });
  }

  return <div className="k-page bs-root devices-page" ref={panel}>
    <PageHeader title="Devices" actions={<Button variant="primary" disabled={busy || !fresh || !settings?.pairingUrl} onClick={issue}>Pair a device</Button>} />
    {(loadError || (!code && error)) && <Alert tone="error">{loadError || error}</Alert>}
    <Section title="Remote access">
      {settings ? <Card><Row title={signIn} meta={!settings.canRequire ? 'Pair this browser first.' : undefined}
        trailing={<Switch label={signIn} checked={settings.required} disabled={busy || !fresh || !settings.canRequire} onChange={toggle} />} /></Card> : <SkeletonRows count={1} />}
      {settings && !settings.pairingUrl && <Alert>Set up an HTTPS address to pair a device.</Alert>}
    </Section>
    <Section title="Paired devices">
      {!settings ? <SkeletonRows /> : settings.devices.length ? <Card>{settings.devices.map(device =>
        <Row key={device.id} title={device.name} leading={<Smartphone size={16} aria-hidden />}
          meta={<>{settings.current === device.id && 'This browser · '}Last seen <time dateTime={new Date(device.lastSeen).toISOString()}>{lastSeen.format(device.lastSeen)}</time></>}
          trailing={<Button size="sm" aria-label={`Revoke ${device.name}`} disabled={busy} onClick={() => { void revoke(device); }}>Revoke</Button>} />)}</Card> : <EmptyState title="No paired devices" />}
    </Section>
    {code && <Dialog open title="Pair a device" onOpenChange={open => { if (!open && !busy) { setCode(undefined); setError(''); } }}
      footer={expired ? <Button variant="primary" busy={busy} onClick={issue}>Create new code</Button> : <Button onClick={() => setCode(undefined)}>Done</Button>}>
      {error && <Alert tone="error">{error}</Alert>}
      {expired ? <Alert>Pairing code expired.</Alert> : <div className="devices-pairing">
        <img src={code.qr} alt="QR code for the Minarbot pairing page" width="160" height="160" />
        <strong aria-label={`Pairing code ${code.code.split('').join(' ')}`}>{code.code}</strong>
        <a className="k-link" href={code.url} target="_blank" rel="noopener noreferrer">{code.url}</a>
        <span className="k-field__hint">Valid for 2 minutes</span>
      </div>}
    </Dialog>}
  </div>;
}

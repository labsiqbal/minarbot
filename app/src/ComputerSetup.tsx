import { useCallback, useRef, useState } from 'react';
import { Monitor } from 'lucide-react';
import { Alert, Button, Card, PageHeader, Row, SkeletonRows } from './ui/kit';
import { useVisiblePolling } from './hooks/useVisiblePolling';
import type { ComputerRequest } from './AgentComputerPanel';

type ComputerSetupStatus = {
  state: 'no_runtime' | 'image_missing' | 'downloading' | 'ready' | 'failed';
  runtime: 'Podman' | 'Docker' | null;
  missingPrograms: string[];
  canSetup: boolean;
  message: string | null;
};
export type { ComputerSetupStatus };

const labels = {
  no_runtime: 'No container runtime',
  image_missing: 'Image missing',
  downloading: 'Downloading image',
  ready: 'Ready',
  failed: 'Setup failed',
};

export function computerSetupLabel(status: ComputerSetupStatus) {
  if (status.state === 'downloading' && !status.runtime) return 'Setting up Computer';
  return labels[status.state];
}

export function ComputerSetup({ request, onboarding = false }: { request: ComputerRequest; onboarding?: boolean }) {
  const [status, setStatus] = useState<ComputerSetupStatus>();
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const flight = useRef(0);
  const ref = useVisiblePolling(useCallback(async current => {
    const version = flight.current;
    if (version % 2) return;
    try {
      const next = await request<ComputerSetupStatus>('/api/settings/computer');
      if (current() && version === flight.current) {
        setStatus(next);
        setError('');
      }
    } catch {
      if (current() && version === flight.current) setError('Computer status could not load.');
    }
  }, [request]), status?.state === 'downloading' || busy ? 1000 : 5000);

  async function setup() {
    if (flight.current % 2) return;
    flight.current++;
    setBusy(true);
    setError('');
    try { setStatus(await request<ComputerSetupStatus>('/api/settings/computer', {})); }
    catch (error) { setError(error instanceof Error ? error.message : 'Computer setup could not start.'); }
    finally { flight.current++; setBusy(false); }
  }

  return <div ref={ref} className="k-stack">
    {status ? <Card>
      <Row title={onboarding ? 'Computer (optional)' : 'This device'} leading={<Monitor size={16} aria-hidden />}
        meta={<span role="status">{computerSetupLabel(status)}{status.runtime && status.state === 'ready' ? ` · ${status.runtime}` : ''}</span>}
        trailing={status.canSetup && <Button variant={onboarding ? 'secondary' : 'primary'} busy={busy || status.state === 'downloading'} onClick={() => void setup()}>Set up Computer</Button>}>
        {status.missingPrograms.length > 0 && <span className="k-row__meta">Missing programs - {status.missingPrograms.join(', ')}</span>}
      </Row>
    </Card> : !error && <SkeletonRows count={1} />}
    {(error || status?.message) && <Alert tone="error">{error || status?.message}</Alert>}
  </div>;
}

const focusHeading = (heading: HTMLHeadingElement | null) => heading?.focus();

export function ComputerSetupPage({ request }: { request: ComputerRequest }) {
  return <div className="k-page bs-root">
    <PageHeader headingRef={focusHeading} title="Computer" />
    <ComputerSetup request={request} />
  </div>;
}

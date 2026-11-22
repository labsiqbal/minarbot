import { useId } from 'react';
import { Info } from 'lucide-react';
import { useCachedQuery } from './hooks/useCachedQuery';
import { Alert, Badge, Button, Card, IconButton, Row, Section } from './ui/kit';

export function LibraryDocsSettings({ request }: { request: <T>(path: string) => Promise<T> }) {
  const id = useId();
  const { data, busy, error, reload } = useCachedQuery<{ reachable: boolean; keySet: boolean }>(request, '/api/settings/library-docs');
  return <Section id={id} title="Library docs" actions={<Button size="sm" disabled={busy} onClick={reload}>Check status</Button>}>
    <Card>
      <Row title="Context7" trailing={<>
        {!data && <span className="settings-fact">{busy ? 'Checking…' : 'Unknown'}</span>}
        <span role="status">{data && <Badge tone={data.reachable ? 'success' : 'warn'}>{data.reachable ? 'Reachable' : 'Unavailable'}</Badge>}</span>
      </>} />
      <Row title="Context7 API key" trailing={<>
        <span className="settings-fact">{data ? data.keySet ? 'Set in Vault' : 'Optional' : 'Unknown'}</span>
        <IconButton label="In Vault, add a Secret named context7-api-key for https://context7.com and leave agents unselected." aria-label="How to set a Context7 API key"><Info size={14} aria-hidden /></IconButton>
      </>} />
    </Card>
    {!!error && <Alert tone="error">Could not check library docs status.</Alert>}
  </Section>;
}

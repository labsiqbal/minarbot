import { useId, useRef, useState } from 'react';
import { Alert, Button, Card, Input, Row, Section, SkeletonRows } from './ui/kit';
import { useWebSearchSettings, webSearchSettingsPath, type WebSearchConfig } from './hooks/useWebSearchSettings';
import './ui/web-search-settings.css';

export function WebSearchSettings({ request }: { request: <T>(path: string, data?: unknown) => Promise<T> }) {
  const id = useId();
  const field = useRef<HTMLInputElement>(null);
  const { settings, setSettings, loadError } = useWebSearchSettings(request);
  const [draft, setDraft] = useState<string>();
  const [busy, setBusy] = useState<'test' | 'save' | null>(null);
  const [count, setCount] = useState<number | null>(null);
  const [error, setError] = useState('');
  const [saved, setSaved] = useState(false);

  const url = draft ?? settings?.url ?? '';

  async function apply(action: 'test' | 'save') {
    if (!settings || settings.managed || busy || !field.current?.reportValidity()) return;
    setBusy(action);
    setError('');
    setSaved(false);
    try {
      if (action === 'test') {
        setCount(null);
        setCount((await request<{ count: number }>(webSearchSettingsPath + '/test', { id: crypto.randomUUID(), url })).count);
      } else {
        setSettings(await request<WebSearchConfig>(webSearchSettingsPath, { id: crypto.randomUUID(), url }));
        setDraft(undefined);
        setSaved(true);
      }
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : 'Web search request failed.');
    } finally {
      setBusy(null);
    }
  }

  return <Section id={id} title="Web search">
    {!settings ? (loadError ? <Alert tone="error">{loadError}</Alert> : <SkeletonRows count={1} />)
      : settings.managed && !settings.url ? <Card><Row title="Backend" trailing={<span className="settings-fact">Set by server</span>} /></Card>
      : <Card>
        <form className="web-search-settings" aria-labelledby={id} onSubmit={event => {
          event.preventDefault();
          void apply('save');
        }}>
          <Row title={<label htmlFor={id + '-url'}>SearXNG URL</label>} meta={settings.managed ? undefined : <>
            <span role="status">{count === null ? '' : count + (count === 1 ? ' result' : ' results')}</span>
            <span className="sr-only" role="status">{saved ? 'Web search settings saved.' : ''}</span>
            {error && <span id={id + '-error'} className="k-field__error" role="alert">{error}</span>}
          </>} trailing={<>
            <Input id={id + '-url'} name="url" ref={field} type="url" maxLength={4096} autoComplete="off" autoCapitalize="none" spellCheck={false}
              placeholder="http://127.0.0.1:8890" value={url} readOnly={settings.managed || busy !== null}
              aria-describedby={error ? id + '-error' : undefined}
              onChange={event => {
                setDraft(event.target.value);
                setCount(null);
                setError('');
                setSaved(false);
              }} />
            {settings.managed ? <span className="settings-fact">Set by server</span> : <>
              <Button size="sm" busy={busy === 'test'} disabled={busy !== null || !url.trim()} onClick={() => void apply('test')}>Test</Button>
              <Button size="sm" variant="primary" type="submit" busy={busy === 'save'} disabled={busy !== null || url.trim() === (settings.url ?? '')}>Save</Button>
            </>}
          </>} />
        </form>
      </Card>}
  </Section>;
}

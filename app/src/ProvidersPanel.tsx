import { useCachedQuery } from './hooks/useCachedQuery';
import { useEffect, useId, useRef, useState } from 'react';
import { Ellipsis, Plus, Server } from 'lucide-react';
import { ActionMenu, Alert, Delayed, Badge, Button, Card, Collapsible, Dialog, EmptyState, Field, IconButton, Input, PageHeader, Row, Section, Select, SkeletonRows, useConfirm } from './ui/kit';

const path = '/api/provider-accounts';
const providers = {
  'openai-compatible': { label: 'OpenAI-compatible', baseURL: 'https://api.openai.com/v1' },
  anthropic: { label: 'Anthropic', baseURL: 'https://api.anthropic.com/v1' },
  gemini: { label: 'Gemini', baseURL: 'https://generativelanguage.googleapis.com/v1beta' },
};
type Provider = keyof typeof providers;
type Request = <T>(path: string, data?: unknown) => Promise<T>;
export type ProviderAccount = { id: string; name: string; host: string; configured: boolean; defaultModel: string; provider: Provider } & (
  { source: 'environment' } | { source: 'settings'; baseURL: string; revision: number }
);
type Account = ProviderAccount;
type SavedAccount = Extract<Account, { source: 'settings' }>;
const message = (error: unknown) => error instanceof Error ? error.message : 'Request failed. Try again.';

export function ProvidersPanel({ request }: { request: Request }) {
  const { data: accounts, setData: setAccounts, fresh, error: loadError, reload } = useCachedQuery<Account[]>(request, path);
  const [actionError, setError] = useState('');
  const error = actionError || (loadError ? message(loadError) : '');
  const [editing, setEditing] = useState<SavedAccount | 'new' | null>(null);
  const [removing, setRemoving] = useState(false);
  const heading = useRef<HTMLHeadingElement>(null);
  const add = useRef<HTMLButtonElement>(null);
  const confirm = useConfirm();
  useEffect(() => { heading.current?.focus(); }, []);

  async function remove(account: SavedAccount) {
    setRemoving(true);
    try {
      if (!await confirm({ title: 'Remove provider?', description: account.name, confirm: 'Remove', danger: true })) return;
      setError('');
      await request(`${path}/${account.id}/delete`, { id: crypto.randomUUID() });
      setAccounts(current => current?.filter(item => item.id !== account.id) ?? null);
      add.current?.focus();
    } catch (error) { setError(message(error)); }
    finally { setRemoving(false); }
  }
  function saved(result: SavedAccount) {
    setAccounts(current => current?.some(account => account.id === result.id)
      ? current.map(account => account.id === result.id ? result : account)
      : [...current ?? [], result]);
    setEditing(null);
  }

  const visible = accounts?.filter(account => account.configured || account.source === 'settings');
  return <div className="k-page bs-root">
    <PageHeader headingRef={heading} title="Providers" actions={<Button ref={add} variant="primary" disabled={accounts === null} icon={<Plus size={14} aria-hidden />} onClick={() => { setError(''); setEditing('new'); }}>Add provider</Button>} />
    {error && <Alert tone="error" actions={!accounts && <Button size="sm" onClick={() => { setError(''); reload(); }}>Retry</Button>}>{error}</Alert>}
    {!visible ? !error && <SkeletonRows /> : !visible.length ? <EmptyState icon={<Server size={20} aria-hidden />} title="No providers" /> : visible.map(account => <Section key={account.id} title={account.name} actions={account.source === 'settings'
      ? <ActionMenu label={`Actions for ${account.name}`} trigger={<IconButton label={`Actions for ${account.name}`} disabled={removing || !fresh}><Ellipsis size={16} aria-hidden /></IconButton>} items={[
        { label: 'Edit', onSelect: () => { setError(''); setEditing(account); } },
        { label: 'Remove', danger: true, onSelect: () => { void remove(account); } },
      ]} />
      : <Badge>From server environment</Badge>}>
      <Card>
        <Row title="Host" trailing={<><span className="provider-fact">{account.host || 'Not configured'}</span><Badge tone={account.configured ? 'success' : undefined}>{account.configured ? 'Configured' : 'Not configured'}</Badge></>} />
        <ProviderModels key={account.source === 'settings' ? account.revision : account.id} account={account} request={request} />
      </Card>
    </Section>)}
    {editing && <ProviderForm account={editing === 'new' ? undefined : editing} request={request} onClose={() => setEditing(null)} onSaved={saved} />}
  </div>;
}

export function ProviderForm({ account, request, onClose, onSaved }: { account?: SavedAccount; request: Request; onClose?: () => void; onSaved: (account: SavedAccount) => void }) {
  const form = useId();
  const [id] = useState(() => crypto.randomUUID());
  const [provider, setProvider] = useState<Provider>(account?.provider ?? 'openai-compatible');
  const [baseURL, setBaseURL] = useState(account?.baseURL ?? providers[provider].baseURL);
  const [defaultModel, setDefaultModel] = useState(account?.defaultModel ?? '');
  const [models, setModels] = useState<string[] | null>(null);
  const [busy, setBusy] = useState<'test' | 'save' | null>(null);
  const [error, setError] = useState('');
  const endpoint = account ? `${path}/${account.id}` : path;
  const canSave = models?.includes(defaultModel) ?? false;

  function connectionChanged() {
    setModels(null);
    setError('');
  }

  const actions = <>
    <Button type="submit" form={form} name="action" value="test" busy={busy === 'test'} disabled={!!busy}>Test</Button>
    <Button type="submit" form={form} name="action" value="save" variant="primary" busy={busy === 'save'} disabled={!!busy || !canSave}>Save</Button>
  </>;
  const fields = <form id={form} className="k-stack" onSubmit={event => {
      event.preventDefault();
      if (busy) return;
      const action = (event.nativeEvent as SubmitEvent).submitter?.getAttribute('value') === 'save' ? 'save' : 'test';
      if (action === 'save' && !canSave) return;
      const fields = new FormData(event.currentTarget);
      const body = { id, provider, baseURL, name: fields.get('name'), apiKey: fields.get('apiKey') };
      setBusy(action);
      setError('');
      if (action === 'test') {
        setModels(null);
        void request<{ models: string[] }>(`${endpoint}/test`, body).then(({ models }) => {
          setModels(models);
          setDefaultModel(current => models.includes(current) ? current : models[0] ?? '');
        }).catch(error => setError(message(error))).finally(() => setBusy(null));
      } else {
        void request<SavedAccount>(account ? `${endpoint}/edit` : endpoint, { ...body, defaultModel, ...(account && { revision: account.revision }) })
          .then(onSaved).catch(error => setError(message(error))).finally(() => setBusy(null));
      }
    }}>
      <Field label="Type"><Select value={provider} disabled={!!busy} options={Object.entries(providers).map(([value, { label }]) => ({ value, label }))} onChange={value => {
        const next = value as Provider;
        setProvider(next); setBaseURL(providers[next].baseURL); connectionChanged();
      }} /></Field>
      <Field label="Name"><Input name="name" required maxLength={80} defaultValue={account?.name} readOnly={!!busy} onChange={() => setError('')} /></Field>
      <Field label="Base URL"><Input name="baseURL" type="url" required maxLength={2048} value={baseURL} readOnly={!!busy} spellCheck={false} autoCapitalize="none" onChange={event => { setBaseURL(event.target.value); connectionChanged(); }} /></Field>
      <Field label="API key" hint={account && 'Blank keeps the saved key.'}><Input name="apiKey" type="password" required={!account} maxLength={8192} autoComplete="new-password" readOnly={!!busy} onChange={connectionChanged} /></Field>
      <Field label="Default model"><Select value={models ? defaultModel : ''} disabled={!!busy || !models?.length} options={models?.length ? models.map(model => ({ value: model, label: model })) : [{ value: '', label: account?.defaultModel || 'Test to load models' }]} onChange={setDefaultModel} /></Field>
      {models && <span role="status" className="k-field__hint">{models.length ? `${models.length} models found` : 'No models found'}</span>}
      {error && <Alert tone="error">{error}</Alert>}
      {!onClose && <div className="k-row-actions">{actions}</div>}
    </form>;
  return onClose ? <Dialog open title={account ? 'Edit provider' : 'Add provider'} onOpenChange={open => { if (!open && !busy) onClose(); }} footer={actions}>{fields}</Dialog> : fields;
}

function ProviderModels({ account, request }: { account: Account; request: Request }) {
  const [query, setQuery] = useState(''), [closed, setClosed] = useState<string[]>([]);
  const { data, error, reload } = useCachedQuery<{ models: string[] }>(request, `/api/models?account=${encodeURIComponent(account.id)}`, account.configured);
  const models = data?.models;
  const groups = new Map<string, string[]>();
  for (const model of [...new Set(models ?? [])].sort()) {
    if (!model.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase())) continue;
    const prefix = model.includes('/') ? `${model.split('/')[0]}/` : 'Other';
    const items = groups.get(prefix) ?? [];
    items.push(model); groups.set(prefix, items);
  }
  return <>
    <Row title="Default model" trailing={<span className="provider-fact provider-model-id">{account.defaultModel || 'None'}</span>} />
    {account.configured && <div className="provider-models">
      <div className="provider-models__head"><span>Models</span>{models && <Badge>{models.length}</Badge>}</div>
      <Input type="search" aria-label={`Search models for ${account.name}`} placeholder="Search models" value={query} onChange={event => { setQuery(event.target.value); setClosed([]); }} />
      {error != null && <Alert tone="error" actions={<Button size="sm" onClick={reload}>Retry</Button>}>{error instanceof Error ? error.message : 'Models could not be loaded.'}</Alert>}
      {!models ? !error && <Delayed><p role="status">Loading models...</p></Delayed> : <div className="provider-models__scroll" role="region" aria-label={`Models for ${account.name}`} tabIndex={0}>
        {[...groups].map(([prefix, items]) => <Collapsible key={prefix} open={!closed.includes(prefix)} onOpenChange={open => setClosed(current => open ? current.filter(item => item !== prefix) : [...current, prefix])} title={<>{prefix}<Badge>{items.length}</Badge></>}>
          <ul className="provider-models__list">{items.map(model => <li key={model}><span className="provider-model-id">{model}</span>{model === account.defaultModel && <Badge>Default</Badge>}</li>)}</ul>
        </Collapsible>)}
        {!groups.size && <p role="status">{query.trim() ? 'No matching models' : 'No models'}</p>}
      </div>}
    </div>}
  </>;
}

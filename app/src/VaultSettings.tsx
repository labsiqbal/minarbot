import { useId, useState, type FormEvent } from 'react';
import { KeyRound, Plus } from 'lucide-react';
import type { ComputerRequest } from './AgentComputerPanel';
import { useVaultSettings, type VaultEntry } from './hooks/useVaultSettings';
import { Alert, Button, Card, Checkbox, Dialog, EmptyState, Field, Input, PageHeader, Row, Select, SkeletonRows, Textarea, useConfirm } from './ui/kit';

type Agent = { id: string; name: string };
type Entry = VaultEntry;

function EntryDialog({ entry, agents, request, close, update }: { entry?: Entry; agents: Agent[]; request: ComputerRequest; close: () => void; update: (entries: Entry[]) => void }) {
  const [kind, setKind] = useState<Entry['kind']>(entry?.kind ?? 'login');
  const [access, setAccess] = useState(entry?.agents ?? []);
  const [clearTotp, setClearTotp] = useState(false), [clearNotes, setClearNotes] = useState(false);
  const [busy, setBusy] = useState(false), [error, setError] = useState('');
  const formId = useId();
  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (busy) return;
    const form = event.currentTarget;
    const fields = new FormData(form);
    const input: Record<string, unknown> = { name: entry?.name ?? fields.get('name'), kind, agents: access, revision: entry?.revision ?? 0 };
    for (const field of ['url', 'username', 'password', 'totp', 'notes', 'value']) {
      const value = fields.get(field);
      if (value) input[field] = value;
    }
    if (clearTotp) input.totp = '';
    if (clearNotes) input.notes = '';
    setBusy(true);
    setError('');
    try {
      update(await request<Entry[]>('/api/vault', input));
      close();
    } catch (error) {
      setError(error instanceof Error ? error.message : 'Vault entry could not be saved.');
    } finally {
      setBusy(false);
    }
  }
  return <Dialog open onOpenChange={open => { if (!open && !busy) close(); }} title={entry ? `Edit ${entry.name}` : 'Add Vault entry'}
    footer={<><Button disabled={busy} onClick={close}>Cancel</Button><Button form={formId} type="submit" variant="primary" busy={busy}>Save entry</Button></>}>
    <form id={formId} className="k-stack" onSubmit={event => void save(event)} autoComplete="off">
      {error && <Alert tone="error">{error}</Alert>}
      {!entry && <>
        <Field label="Type"><Select value={kind} onChange={value => setKind(value === 'secret' ? 'secret' : 'login')} options={[{ value: 'login', label: 'Login' }, { value: 'secret', label: 'Secret' }]} /></Field>
        <Field label="Entry name" hint="Lowercase letters, numbers, and hyphens."><Input name="name" required maxLength={64} pattern="(?:[a-z0-9]|-){1,64}" spellCheck={false} /></Field>
      </>}
      <Field label="Site URL"><Input name="url" type="url" required={!entry} maxLength={2000} placeholder={entry?.origin} spellCheck={false} /></Field>
      {kind === 'login' ? <>
        <Field label="Username"><Input name="username" required={!entry} maxLength={4000} placeholder={entry && 'Leave blank to keep saved username'} spellCheck={false} /></Field>
        <Field label="Password"><Input name="password" type="password" required={!entry} maxLength={4000} placeholder={entry && 'Leave blank to keep saved password'} autoComplete="new-password" /></Field>
        <Field label="TOTP key" optional><Input name="totp" type="password" disabled={clearTotp} maxLength={256} placeholder={entry?.hasTotp ? 'Leave blank to keep saved key' : undefined} autoComplete="new-password" /></Field>
        {entry?.hasTotp && <Checkbox label="Remove saved TOTP key" checked={clearTotp} onChange={setClearTotp} />}
      </> : <Field label="Secret value"><Input name="value" type="password" required={!entry} maxLength={4000} placeholder={entry && 'Leave blank to keep saved value'} autoComplete="new-password" /></Field>}
      <Field label="Notes" optional><Textarea name="notes" rows={2} disabled={clearNotes} maxLength={4000} placeholder={entry?.hasNotes ? 'Leave blank to keep saved notes' : undefined} /></Field>
      {entry?.hasNotes && <Checkbox label="Remove saved notes" checked={clearNotes} onChange={setClearNotes} />}
      {!!agents.length && <fieldset className="k-fieldset">
        <legend className="k-field__label">Agent access</legend>
        {agents.map(agent => <Checkbox key={agent.id} label={agent.name} checked={access.includes(agent.id)} onChange={checked => {
          setAccess(current => checked ? [...current, agent.id] : current.filter(id => id !== agent.id));
        }} />)}
      </fieldset>}
    </form>
  </Dialog>;
}

export function VaultSettings({ agents, request }: { agents: Agent[]; request: ComputerRequest }) {
  const [editing, setEditing] = useState<Entry | 'new' | null>(null);
  const [reload, setReload] = useState(0), [deleting, setDeleting] = useState<string | null>(null);
  const { entries, setEntries, error, setError } = useVaultSettings(request, reload);
  const confirm = useConfirm();

  async function remove(entry: Entry) {
    if (!await confirm({ title: `Delete ${entry.name}?`, description: 'Agents will lose access to this Vault entry.', confirm: 'Delete entry', danger: true })) return;
    setDeleting(entry.name);
    setError('');
    try { setEntries(await request<Entry[]>('/api/vault/delete', { name: entry.name, revision: entry.revision })); }
    catch (error) { setError(error instanceof Error ? error.message : 'Vault entry could not be deleted.'); }
    finally { setDeleting(null); }
  }

  return <div className="k-page bs-root">
    <PageHeader title="Vault" actions={<Button variant="primary" icon={<Plus size={15} aria-hidden />} disabled={!entries || !!deleting} onClick={() => setEditing('new')}>Add entry</Button>} />
    {error && <Alert tone="error" actions={<Button size="sm" onClick={() => setReload(value => value + 1)}>Reload</Button>}>{error}</Alert>}
    {!entries ? !error && <SkeletonRows count={2} /> : entries.length === 0 ? <EmptyState title="No Vault entries" /> : <Card>{entries.map(entry =>
      <Row key={entry.name} title={entry.name} meta={<>{entry.kind === 'login' ? 'Login' : 'Secret'}{entry.origin && <span>{entry.origin}</span>}<span>{entry.agents.length} {entry.agents.length === 1 ? 'agent' : 'agents'}</span></>} leading={<KeyRound size={16} aria-hidden />}
        onClick={() => setEditing(entry)} disabled={!!deleting} trailing={<Button size="sm" variant="ghost" aria-label={`Delete ${entry.name}`} busy={deleting === entry.name} disabled={!!deleting} onClick={() => void remove(entry)}>Delete</Button>} />
    )}</Card>}
    {editing && <EntryDialog entry={editing === 'new' ? undefined : editing} agents={agents} request={request} close={() => setEditing(null)} update={setEntries} />}
  </div>;
}

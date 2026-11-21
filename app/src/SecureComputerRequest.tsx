import { useId, useState, type FormEvent } from 'react';
import type { ComputerWait } from '../shared/computer';
import type { ComputerRequest } from './AgentComputerPanel';
import { Alert, Button, Dialog, Field, Input } from './ui/kit';

export function SecureComputerRequest({ agent, wait, request, disabled }: { agent: string; wait: ComputerWait; request: ComputerRequest; disabled?: boolean }) {
  const [open, setOpen] = useState(false), [busy, setBusy] = useState(false), [error, setError] = useState('');
  const formId = useId();

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (busy) return;
    const form = event.currentTarget;
    const value = new FormData(form).get('value');
    setBusy(true);
    setError('');
    try {
      await request('/api/vault/respond', { agent, turn: wait.turn, call: wait.call_id, value });
      form.reset();
      setOpen(false);
    } catch (error) {
      setError(error instanceof Error ? error.message : 'Secure fill could not be confirmed.');
    } finally { setBusy(false); }
  }

  return <>
    <Button size="sm" variant="primary" disabled={disabled} onClick={() => { setError(''); setOpen(true); }}>Enter value</Button>
    {open && <Dialog open onOpenChange={next => { if (!busy) setOpen(next); }} title="Enter a secure value"
      description={wait.secret_origin}
      footer={<><Button disabled={busy} onClick={() => setOpen(false)}>Cancel</Button><Button type="submit" form={formId} variant="primary" busy={busy}>Save and fill</Button></>}>
      <form id={formId} className="k-stack" autoComplete="off" onSubmit={event => void submit(event)}>
        {error && <Alert tone="error">{error}</Alert>}
        <Field label={wait.secret_entry}><Input name="value" type="password" autoComplete="new-password" required maxLength={4000} /></Field>
      </form>
    </Dialog>}
  </>;
}

import { useRef, useState } from 'react';
import { ShieldCheck } from 'lucide-react';
import { Alert, Badge, Button, Card, CodeBlock, Collapsible, Row } from './ui/kit';
export type HarnessApproval = { id: string; kind: string; description: string; command: string | null; prefix: string | null; input: unknown; state: string; decision: string | null; revision: number };
export function HarnessApprovalCard({ approval, path, status, archived, request }: { approval: HarnessApproval; path: string; status: string; archived: boolean; request: <T>(path: string, data?: unknown) => Promise<T> }) {
  const key = `minarbot:harness-approval:${path}:${approval.id}`;
  const [error, setError] = useState(''), [busy, setBusy] = useState(false);
  const pending = useRef<{ id: string; revision: number; decision: string } | null>(null);
  const open = approval.state === 'approval', disabled = busy || archived || status !== 'running';
  const email = approval.kind === 'email';
  const craft = approval.kind === 'craft';
  const draft = email ? approval.input as { from: string; to: string; subject: string; text: string } | null : null;
  async function decide(decision: string) {
    if (disabled || !open) return;
    setBusy(true); setError('');
    try {
      const saved = sessionStorage.getItem(key);
      const command = pending.current ?? (saved ? JSON.parse(saved) : { id: crypto.randomUUID(), revision: approval.revision, decision });
      if (!command || typeof command.id !== 'string' || command.revision !== approval.revision || !['allow','always','deny'].includes(command.decision)) throw new Error('Saved approval expired. Reload the transcript.');
      pending.current = command; sessionStorage.setItem(key, JSON.stringify(command));
      await request(`${path}/harness-approvals/${approval.id}`, command);
      sessionStorage.removeItem(key); pending.current = null;
    } catch (error) { setError(error instanceof Error ? error.message : 'Decision not confirmed. Retry the same decision.'); }
    finally { setBusy(false); }
  }
  const outcome = approval.state === 'expired' ? 'Expired; ask again' : approval.decision === 'deny' ? 'Not allowed' : email ? 'Draft saved' : approval.decision === 'always' ? 'Always allowed for this command prefix' : 'Allowed once';
  return <section className="bs-root k-stack" aria-label={email ? 'Email draft approval' : craft ? 'Craft Anything approval' : 'Coding harness approval'}><Card><div className="k-stack" style={{ padding: 14 }}>
    <Row leading={<ShieldCheck size={18} aria-hidden />} title={<h3 className="k-section__title">{email ? 'Email draft' : craft ? 'Craft Anything permission' : 'Coding harness permission'}</h3>} trailing={<Badge tone={open ? 'warn' : 'neutral'}>{open ? 'Approval needed' : outcome}</Badge>} />
    <p className="message-plain" style={{ margin: 0, overflowWrap: 'anywhere' }}>{approval.description}</p>
    {approval.command && <CodeBlock label="Shell command">{approval.command}</CodeBlock>}
    {email ? draft && <CodeBlock label="Email preview">{`From: ${draft.from}\nTo: ${draft.to}\nSubject: ${draft.subject}\n\n${draft.text}`}</CodeBlock> : <Collapsible title="Request details"><CodeBlock label="Harness request arguments">{JSON.stringify(approval.input, null, 2)}</CodeBlock></Collapsible>}
    {approval.prefix && <Collapsible title="Always allow scope"><p className="k-field__hint">Applies to this agent, runtime, workspace, and literal command prefix: <code>{approval.prefix}</code>. Switching runtime clears these grants.</p></Collapsible>}
    {error && <Alert tone="error" actions={pending.current && <Button size="sm" disabled={disabled} onClick={() => void decide(pending.current!.decision)}>Retry saved decision</Button>}>{error} Retry keeps the original decision and request ID.</Alert>}
    {open && <div className="k-row-actions"><Button size="sm" disabled={disabled || !!pending.current} onClick={() => void decide('deny')}>Don't allow</Button>{approval.prefix && <Button size="sm" disabled={disabled || !!pending.current} onClick={() => void decide('always')}>Always allow for this agent + command prefix</Button>}<Button size="sm" variant="primary" disabled={disabled || !!pending.current} onClick={() => void decide('allow')}>{email ? 'Approve draft' : 'Allow once'}</Button></div>}
  </div></Card></section>;
}

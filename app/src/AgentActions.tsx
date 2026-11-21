import { useRef, useState, type ReactNode } from 'react';
import { flushSync } from 'react-dom';
import { DropdownMenu } from 'radix-ui';
import { Ellipsis, Eye, EyeOff, Inbox, Pencil, Trash2 } from 'lucide-react';
import type { Agent } from './AgentProfileDialog';
import { Button, IconButton, useConfirm } from './ui/kit';

type Request = <T>(path: string, body?: unknown) => Promise<T>;

export function HiddenAgentBanner({ agent, request, onChanged }: { agent: Agent; request: Request; onChanged?: () => Promise<void> }) {
  const [busy, setBusy] = useState(false), [error, setError] = useState('');
  async function restore() {
    if (busy) return;
    setBusy(true); setError('');
    try {
      await request(`/api/agents/${agent.id}/visibility`, { id: crypto.randomUUID(), revision: agent.visibility_revision ?? 0, hidden: false });
      await onChanged?.();
    } catch (error) { setError(error instanceof Error ? error.message : 'Restore failed. Try again.'); }
    finally { setBusy(false); }
  }
  return <div className="composer__note">Hidden <Button size="sm" variant="ghost" busy={busy} onClick={() => void restore()}>Restore</Button>{error && <span role="alert">{error}</span>}</div>;
}

export function useDeleteAgent(request: Request, onDeleted: (id: string) => Promise<void>) {
  const confirm = useConfirm(), flight = useRef(false);
  const [deleting, setDeleting] = useState('');
  return { deleting, deleteAgent: async (agent: Agent) => {
    if (flight.current) return;
    flight.current = true;
    try {
      if (!await confirm({ title: `Delete ${agent.name}?`, description: 'Permanently removes agent data and access.', confirm: 'Delete agent', danger: true })) return;
      flushSync(() => setDeleting(agent.id));
      await request(`/api/agents/${agent.id}/delete`, { id: crypto.randomUUID() });
      await onDeleted(agent.id);
    } finally { setDeleting(''); flight.current = false; }
  } };
}

export function AgentActions({ agent, children, request, onEdit, onInbox, onDelete, onChanged, onError }: {
  agent: Agent; children: ReactNode; request: Request; onEdit: () => void; onInbox: () => void;
  onDelete: () => Promise<void>; onChanged: () => Promise<void>; onError: (error: unknown) => void;
}) {
  const [open, setOpen] = useState(false), [busy, setBusy] = useState(false);
  const openingInbox = useRef(false);
  async function act(action: () => Promise<void>) {
    if (busy) return;
    setBusy(true);
    try { await action(); } catch (error) { onError(error); } finally { setBusy(false); }
  }
  return <DropdownMenu.Root open={open} onOpenChange={setOpen}>
    <div className="sidebar-agent" onContextMenu={event => { event.preventDefault(); setOpen(true); }} onKeyDown={event => { if (event.key === 'ContextMenu' || event.shiftKey && event.key === 'F10') { event.preventDefault(); setOpen(true); } }}>
      {children}
      <DropdownMenu.Trigger asChild><IconButton className="agent-actions" label={`Actions for ${agent.name}`}><Ellipsis size={16} aria-hidden /></IconButton></DropdownMenu.Trigger>
    </div>
    <DropdownMenu.Portal><DropdownMenu.Content className="k-pop k-menu" align="end" sideOffset={4} onCloseAutoFocus={event => { if (openingInbox.current) { event.preventDefault(); openingInbox.current = false; } }}>
      <DropdownMenu.Item className="k-menu__item" disabled={busy} onSelect={onEdit}><Pencil size={15} aria-hidden />{agent.builtin ? 'AI connection' : 'Edit'}</DropdownMenu.Item>
      {!agent.builtin && <DropdownMenu.Item className="k-menu__item" disabled={busy} onSelect={() => { openingInbox.current = true; onInbox(); }}><Inbox size={15} aria-hidden />Inbox</DropdownMenu.Item>}
      <DropdownMenu.Item className="k-menu__item" disabled={busy} onSelect={() => void act(async () => { await request(`/api/agents/${agent.id}/visibility`, { id: crypto.randomUUID(), revision: agent.visibility_revision ?? 0, hidden: !agent.hidden }); await onChanged(); })}>{agent.hidden ? <Eye size={15} aria-hidden /> : <EyeOff size={15} aria-hidden />}{agent.hidden ? 'Restore' : 'Hide'}</DropdownMenu.Item>
      {!agent.builtin && <><DropdownMenu.Separator className="k-menu__sep" /><DropdownMenu.Item className="k-menu__item is-danger" disabled={busy} onSelect={() => void act(onDelete)}><Trash2 size={15} aria-hidden />Delete</DropdownMenu.Item></>}
    </DropdownMenu.Content></DropdownMenu.Portal>
  </DropdownMenu.Root>;
}

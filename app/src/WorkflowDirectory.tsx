import { useCachedData, useQueryPolling } from './hooks/useCachedQuery';
import { queryCache } from './hooks/queryCache';
import { useEffect, useRef, useState, type ReactNode } from 'react';
import { WorkflowPanel } from './WorkflowPanel';
import { WorkflowInbox } from './WorkflowInbox';
import type { Workflow } from './WorkflowEditor';
import { ActionMenu, Alert, Badge, Button, Card, Dialog, EmptyState, Field, IconButton, PageHeader, Row, Select, SkeletonRows } from './ui/kit';
import { ChevronLeft, ChevronRight, Ellipsis, Plus, Workflow as WorkflowIcon } from 'lucide-react';

type Agent = { id: string; name: string; model: string };
type Page = { workflows: (Workflow & { owner_name: string })[]; next_after: string | null };
export function WorkflowDirectory({ agents, request, renderConversation }: {
  agents: Agent[]; request: <T>(path: string, data?: unknown) => Promise<T>;
  renderConversation: (thread: string, owner: string) => ReactNode;
}) {
  const [cursors, setCursors] = useState(['']);
  const [deleted, setDeleted] = useState(false);
  const [inbox, setInbox] = useState(false), [creating, setCreating] = useState(false);
  const [refresh, setRefresh] = useState(0), [error, setError] = useState('');
  const [owner, setOwner] = useState(agents[0]?.id ?? '');
  const [opened, setOpened] = useState<Workflow | 'new' | null>(null);
  const heading = useRef<HTMLHeadingElement>(null), after = cursors.at(-1)!;
  const path = `/api/workflows?deleted=${deleted ? 1 : 0}${after ? `&after=${after}` : ''}`;
  const { data: page, fresh } = useCachedData<Page>(path);
  useQueryPolling(async signal => {
    try { await queryCache.refresh(path, () => request<Page>(path)); if (!signal.aborted) setError(''); }
    catch (reason) { if (!signal.aborted) setError(reason instanceof Error ? reason.message : 'Workflows could not load.'); }
  }, { interval: 5000, enabled: !opened && !inbox }, [after, refresh, deleted, request]);
  useEffect(() => { if (!opened && !inbox) heading.current?.focus(); }, [opened, after, inbox]);
  const selectedOwner = agents.find(agent => agent.id === (opened && opened !== 'new' ? opened.agent : owner));
  if (inbox) return <WorkflowInbox request={request} onBack={() => setInbox(false)} />;
  if (opened && selectedOwner) return <WorkflowPanel key={selectedOwner.id} owner={selectedOwner} agents={agents} initial={opened} request={request}
    onNavigate={() => {}} onBack={() => { setOpened(null); setRefresh(value => value + 1); }} renderConversation={renderConversation} />;
  return <div className="k-page bs-root">
    <PageHeader headingRef={heading} title="Workflows" actions={<>
      <ActionMenu label="Workflow options" trigger={<IconButton label="Workflow options"><Ellipsis size={16} aria-hidden /></IconButton>} items={[{ label: 'Results inbox', onSelect: () => setInbox(true) }, { label: deleted ? 'Hide deleted' : 'Show deleted', onSelect: () => { setDeleted(value => !value); setCursors(['']); } }]} />
      {!!agents.length && <Button variant="primary" onClick={() => setCreating(true)} icon={<Plus size={14} aria-hidden />}>New workflow</Button>}
    </>} />
    {creating && <Dialog open title="New workflow" onOpenChange={setCreating} footer={<><Button onClick={() => setCreating(false)}>Cancel</Button><Button variant="primary" onClick={() => { setCreating(false); setOpened('new'); }}>Continue</Button></>}>
      <Field label="Starting agent"><Select value={owner} onChange={setOwner} options={agents.map(agent => ({ value: agent.id, label: agent.name }))} /></Field>
    </Dialog>}
    {error && <Alert tone="error" actions={<Button size="sm" onClick={() => setRefresh(value => value + 1)}>Retry</Button>}>{error}</Alert>}
    <div className="k-stack">
      {!page ? !error && <><span className="sr-only" role="status">Loading workflow list...</span><SkeletonRows /></> : !page.workflows.length ? <EmptyState icon={<WorkflowIcon size={18} />} title="No workflows yet" /> : <Card>{page.workflows.map(workflow => <Row key={workflow.id} title={workflow.name} leading={<WorkflowIcon size={16} aria-hidden />} onClick={() => setOpened(workflow)} disabled={!fresh || !agents.some(agent => agent.id === workflow.agent)} meta={<><span>{workflow.owner_name}</span><Badge>Version {workflow.revision}</Badge><span>{workflow.graph.nodes.length} steps</span></>} />)}</Card>}
      {(cursors.length > 1 || page?.next_after) && <div className="k-pager"><IconButton label="Previous workflows" disabled={cursors.length < 2} onClick={() => setCursors(value => value.slice(0, -1))}><ChevronLeft size={16} aria-hidden /></IconButton><IconButton label="Next workflows" disabled={!page?.next_after} onClick={() => { if (page?.next_after) setCursors(value => [...value, page.next_after!]); }}><ChevronRight size={16} aria-hidden /></IconButton></div>}
    </div>
  </div>;
}

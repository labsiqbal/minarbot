// Panel tabs and surface composition adapted from T3 (MIT); see ../t3/provenance.json.
import { useCallback, useMemo, useRef, useState, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { ArrowLeft, FileDiff, Files, Globe, MessageSquare, Monitor, Users, Wrench, X } from 'lucide-react';
import { Dialog as Sheet } from 'radix-ui';
import { useVisiblePolling } from './hooks/useVisiblePolling';
import type { ComputerStatus } from './AgentComputerPanel';
import { Alert, Badge, Button, IconButton, TabPanel, Tabs } from './ui/kit';
import { PanelResizeHandle, detailsPanel, useResizablePanel } from './ui/resizable';
import { createRightPanelStore, rightPanelWidthKey, type RunTarget } from './right-panel-state';
import { PanelFiles, PanelPreview, type PanelFile, type PanelSite, type OpenFile } from './RightPanelFiles';
import { PanelDiff, type GitStatus } from './RightPanelDiff';
import { ResultURLPreview } from './ResultURLPreview';
import { ComputerViewer } from './ComputerViewer';
import { navigateResult, useResultRoute } from './hooks/useResultRoute';
import { usePanelOverlay } from './hooks/usePanelOverlay';
import { resultTarget } from './navigation';
import { AgentAvatar } from './AgentAvatar';
import { RunInspector } from './RunInspector';
import './right-panel.css';

export type PanelRequest = <T>(path: string, body?: unknown) => Promise<T>;
type Context = { workspace: { name: string; project?: string } | null; computer: string | null; sites: PanelSite[]; previews: PanelFile[] };
export type RightPanelExtension = { value: string; label: ReactNode; content: ReactNode };

export function useRightPanel(scope: string) {
  const store = useMemo(() => createRightPanelStore({ getItem: key => localStorage.getItem(key), setItem: (key, value) => localStorage.setItem(key, value) }), []);
  const [selection, setSelection] = useState<{ scope: string; tab: string; file: OpenFile | null }>(() => ({ scope, tab: store.read(scope).tab, file: null }));
  if (selection.scope !== scope) setSelection({ scope, tab: store.read(scope).tab, file: null });
  const [inspector, setInspector] = useState<{ scope: string; target: RunTarget } | null>(null);
  if (inspector && inspector.scope !== scope) setInspector(null);
  const run = inspector?.scope === scope ? inspector.target : null;
  const result = useResultRoute();
  const tab = result ? result.view === 'file' ? 'files' : result.view === 'computer' ? 'computer' : 'preview' : selection.scope === scope ? selection.tab : store.read(scope).tab;
  const spec = useMemo(() => ({ ...detailsPanel, key: rightPanelWidthKey(scope) }), [scope]);
  const panel = useResizablePanel(spec, 280);
  const select = useCallback((value: string) => { navigateResult(); setSelection(current => ({ ...current, scope, tab: value })); if (value !== 'run') store.select(scope, value); }, [scope, store]);
  const selectFile = useCallback((file: OpenFile | null) => { navigateResult(file && !file.transcribe ? { view: 'file', path: file.path, name: file.name } : undefined); setSelection({ scope, tab: 'files', file: !file?.transcribe && resultTarget({ view: 'file', path: file?.path, name: file?.name }) ? null : file }); store.select(scope, 'files'); }, [scope, store]);
  const openRun = useCallback((target: RunTarget) => {
    setInspector({ scope, target: { ...target } });
    select('run');
  }, [scope, select]);
  return { tab, panel, select, result, run, openRun, file: result?.view === 'file' ? { path: result.path, name: result.name } : selection.scope === scope ? selection.file : null, selectFile };
}

export function RightPanel({ path, name, state, runStatus, request, onClose, details, activity, members, attached = [], attachedCount = attached.length, onAttach, attachDisabled = false, settings, extensions = [] }: {
  path: string; name: string; state: ReturnType<typeof useRightPanel>; runStatus?: string; request: PanelRequest; onClose: () => void;
  details: ReactNode; activity: ReactNode; members?: { id: string; name: string; role?: string; lead?: boolean }[]; attached?: string[]; attachedCount?: number; onAttach?: (file: PanelFile) => void; attachDisabled?: boolean; settings?: ReactNode;
  /** Extra tabs: extensions={[{ value, label, content }]}. */
  extensions?: RightPanelExtension[];
}) {
  const base = `${path}/panel`;
  const [context, setContext] = useState<Context | null>(null), [files, setFiles] = useState<PanelFile[]>([]), [git, setGit] = useState<GitStatus | null>(null);
  const [error, setError] = useState(''), [refresh, setRefresh] = useState(0), [loading, setLoading] = useState(true);
  const [computerAvailable, setComputerAvailable] = useState(false);
  const resultComputer = state.result?.view === 'computer' ? state.result.agent : undefined;
  const root = useVisiblePolling(useCallback(async current => {
    if (!path) { setLoading(false); return; }
    try {
      const [next, rows] = await Promise.all([request<Context>(base), request<PanelFile[]>(`${base}/files`)]);
      if (!current()) return;
      setContext(next); setFiles(rows); setError('');
      const targetComputer = resultComputer ?? next.computer;
      if (targetComputer) {
        const computer = await request<ComputerStatus>(`/api/agents/${targetComputer}/computer`);
        if (current()) { setComputerAvailable(computer.available); if (!computer.available && state.tab === 'computer') state.select('chats'); }
      } else { setComputerAvailable(false); if (state.tab === 'computer') state.select('chats'); }
      if (next.workspace) {
        const status = await request<GitStatus>(`${base}/git`);
        if (current()) setGit(status);
      } else setGit(null);
    } catch (reason) { if (current()) setError(reason instanceof Error ? reason.message : 'Panel could not load.'); }
    finally { if (current()) setLoading(false); }
  }, [base, path, request, refresh, state.tab, state.select, resultComputer]));
  const overlay = usePanelOverlay(state.panel.available);
  const width = overlay ? Math.min(520, window.innerWidth - 32) : state.panel.width;
  const [mount] = useState(() => { const element = document.createElement('div'); element.className = 'right-panel-mount'; return element; });
  const place = useCallback((element: HTMLDivElement | null) => { if (element) element.appendChild(mount); }, [mount]);
  const inside = useRef(new WeakSet<Event>());
  const chatsLabel = members ? 'Members' : 'Chats';
  const tabs = [
    { value: 'chats', title: chatsLabel, label: <>{members ? <Users size={14} aria-hidden /> : <MessageSquare size={14} aria-hidden />}<span className="right-panel__tab-label">{chatsLabel}</span></> },
    { value: 'files', title: 'Files', label: <><Files size={14} aria-hidden /><span className="right-panel__tab-label">Files</span></> },
    ...((resultComputer || context?.computer) && computerAvailable ? [{ value: 'computer', title: 'Computer', label: <><Monitor size={14} aria-hidden /><span className="right-panel__tab-label">Computer</span></> }] : []),
    ...(state.run ? [{ value: 'run', title: 'Run', label: <><Wrench size={14} aria-hidden /><span className="right-panel__tab-label">Run</span></> }] : []),
    ...extensions,
  ];
  const selected = ['diff', 'preview'].includes(state.tab) ? 'files' : tabs.some(tab => tab.value === state.tab) ? state.tab : 'chats';
  const content = <aside className="details right-panel bs-root" style={{ width: `${width}px` }} aria-label="Conversation details" onPointerDownCapture={event => inside.current.add(event.nativeEvent)}>
    {!overlay && <PanelResizeHandle panel={state.panel} />}
    <div className="right-panel__header"><div>{settings}<IconButton label="Close details" onClick={onClose}><X size={16} aria-hidden /></IconButton></div></div>
    <Tabs label="Conversation panel" value={selected} onChange={state.select} tabs={tabs}>
      {error && <div className="right-panel__error"><Alert tone="error" actions={<Button size="sm" onClick={() => setRefresh(value => value + 1)}>Retry</Button>}>{error}</Alert></div>}
      <TabPanel value="chats">{members ? <div className="right-panel__body"><ul className="right-panel__members" aria-label="Section members">{members.map(member => <li key={member.id}><AgentAvatar agent={member} size={32} /><div><strong>{member.name}</strong><span>{[member.lead ? 'Lead' : 'Member', member.role].filter(Boolean).join(' · ')}</span></div></li>)}</ul></div> : details}{activity}</TabPanel>
      <TabPanel value="files">
        {state.tab === 'diff' && git?.isRepo ? <><div className="right-panel__back"><IconButton label="Back to files" onClick={() => state.select('files')}><ArrowLeft size={15} aria-hidden /></IconButton><span>Source control</span></div><PanelDiff base={base} status={git} request={request} refresh={refresh} /></> : state.tab === 'preview' && state.result?.view === 'url' ? <ResultURLPreview key={state.result.path} target={state.result} computer={computerAvailable ? resultComputer ?? context?.computer ?? undefined : undefined} name={name} request={request} onBack={() => state.select('files')} /> : state.tab === 'preview' ? <PanelPreview onBack={() => state.select('files')} base={base} files={context?.previews ?? []} sites={context?.sites ?? []} request={request} /> : <PanelFiles selected={state.file} onSelect={state.selectFile} onDeleted={() => { setRefresh(value => value + 1); window.dispatchEvent(new Event('minarbot:message-changed')); }} navigation={<>
          {(git?.isRepo || !!context?.previews.length || !!context?.sites.length) && <div className="k-stack">
            {git?.isRepo && <Button className="right-panel__nav-row" variant="ghost" icon={<FileDiff size={14} aria-hidden />} onClick={() => state.select('diff')}>Source control <Badge>{git.changedFiles?.length ?? 0} changed</Badge></Button>}
            {(!!context?.previews.length || !!context?.sites.length) && <Button className="right-panel__nav-row" variant="ghost" icon={<Globe size={14} aria-hidden />} onClick={() => state.select('preview')}>Preview artifacts</Button>}
          </div>}
        </>} attachedCount={attachedCount} attached={attached} onAttach={onAttach} attachDisabled={attachDisabled} key={base} base={base} files={files} workspace={context?.workspace?.name} loading={loading} request={request} />}
      </TabPanel>
      <TabPanel value="computer">{state.result?.view === 'computer' ? <ResultURLPreview key={state.result.path} target={state.result} computer={computerAvailable ? resultComputer ?? context?.computer ?? undefined : undefined} name={name} request={request} onBack={() => state.select('files')} /> : context?.computer && <div className="right-panel__computer"><ComputerViewer agent={context.computer} name={name} request={request} /></div>}</TabPanel>
      {state.run && <TabPanel value="run"><RunInspector key={state.run.turnId} target={state.run} request={request} turnStatus={runStatus} /></TabPanel>}
      {extensions.map(extension => <TabPanel key={extension.value} value={extension.value}>{extension.content}</TabPanel>)}
    </Tabs>
  </aside>;
  return <div ref={state.panel.ref} className="right-panel-slot" style={{ width: overlay ? 0 : `${width}px` }}>
    <div ref={root} className="right-panel-poll" />
    {createPortal(content, mount)}
    {overlay ? <Sheet.Root open onOpenChange={open => { if (!open) onClose(); }}><Sheet.Portal>
      <Sheet.Overlay className="right-panel-backdrop" />
      <Sheet.Content className="right-panel-sheet" aria-describedby={undefined} onOpenAutoFocus={event => { event.preventDefault(); document.querySelector<HTMLElement>('.right-panel-sheet [role=tab][data-state=active]')?.focus(); }} onPointerDownOutside={event => { if (inside.current.has(event.detail.originalEvent)) event.preventDefault(); }} onKeyDownCapture={event => { if ((event.target as HTMLElement).closest('[role="dialog"]') !== event.currentTarget) return; if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); onClose(); } if ((event.metaKey || event.ctrlKey) && event.shiftKey && event.key.toLowerCase() === 'b') { event.preventDefault(); onClose(); } }} onCloseAutoFocus={event => { event.preventDefault(); document.querySelector<HTMLElement>('.message-find input, [aria-label="Show details"]')?.focus(); }}>
        <Sheet.Title className="sr-only">Conversation panel</Sheet.Title><div ref={place} className="right-panel-mount" />
      </Sheet.Content>
    </Sheet.Portal></Sheet.Root> : <div ref={place} className="right-panel-mount" />}
  </div>;
}

import { ConnectionError, HttpError, readJson } from './http';
import { HiddenAgents } from './AgentSidebar';
import { useConversationRequests } from './hooks/useConversationRequests';
import { useRouteThread } from './hooks/useRouteThread';
import { useAppRoute, useRouteFallback } from './hooks/useAppRoute';
import { parseRoute, routePath, type SettingsSection } from './navigation';
import { useNarrowSidebar } from './hooks/useNarrowSidebar';
import { useMessageChanged } from './hooks/useMessageChanged';
import { focusNeedsYouTarget, NeedsYouFocus, notificationMuteKey, requestKey, useNeedsYouFocus, useNeedsYouNotifications, type PendingInput, type TurnDestination } from './hooks/useNeedsYouNotifications';
import { NeedsYouBadge } from './NeedsYou';
import { useOptimisticTurns } from './hooks/useOptimisticTurns';
import { useComposerFocus } from './hooks/useComposerFocus';
import { useQuickSwitcher } from './hooks/useQuickSwitcher';
import { QuickSwitcher, type SwitcherItem } from './ui/QuickSwitcher';
import { usePolling } from './hooks/usePolling';
import { useCachedData, useQueryPolling } from './hooks/useCachedQuery';
import { queryCache } from './hooks/queryCache';
import { useNextFrame } from './hooks/useNextFrame';
import { MessageFind } from './ui/MessageFind';
import { AgentEmailSettings, AgentInbox } from './AgentEmail';
import { AgentActions, HiddenAgentBanner, useDeleteAgent } from './AgentActions';
import { RightPanel, useRightPanel } from './RightPanel';
import type { RunTarget } from './right-panel-state';
import { restoreThreadCommand, type ThreadCommand } from './thread-command';
import './Conversations.css';
import { AgentConversation, RoomMessages, type RoomView, type Exchange } from './ui/AgentConversation';
import { UpdatesPanel } from './UpdatesPanel';
import { SidebarUpdateCapsule } from './SidebarUpdateCapsule';
import { UpdateEverythingDialog } from './UpdateEverythingDialog';
import { useUpdateEverything } from './hooks/useUpdateEverything';
import { HarnessApprovalCard, type HarnessApproval } from './HarnessApprovalCard';
import './ui/csp-nonce';
import { RestoreCheckpoint } from './SourceControl';
import { ComputerViewer, ComputerRequestCard } from './ComputerViewer';
import type { ComputerWait } from '../shared/computer';
import { AgentComputerPanel } from './AgentComputerPanel';
import { restoreChatCommand, restoreChannelCommand, type ChatCommand, type ChannelCommand } from './conversation-command';
import { AgentForm } from './AgentProfileDialog';
import { AgentAvatar } from './AgentAvatar';
import { QueuePanel } from './QueuePanel';
import { useNewConversationShortcut } from './hooks/useNewConversationShortcut';
import { useRoutePresentation } from './hooks/useRoutePresentation';
import { QueueEditor } from './QueueEditor';
import { CancelTurn, QueuedRunsControl } from './ui/RunControls';
import { Markdown, StreamingMarkdown } from './Markdown';
import { HtmlReplies } from './HtmlReply';
import { ComponentFormCard, ComponentReplies, type ComponentReplyData } from './ComponentReply';
import type { HtmlReplyAction, HtmlReplyContext, HtmlReplyReference } from './html-reply';
import { MessageCopyButton } from './ui/MessageCopyButton';
import { useTimelineFollow } from './ui/useTimelineFollow';
import { useTurnStream } from './ui/useTurnStream';
import { SpeechPlayback } from './SpeechPlayback';
import { useConversationAttachments } from './ConversationAttachments';
import { Attachments } from './Attachments';
import { ProvidersPanel } from './ProvidersPanel';
import { EnginesPanel, isLoginBinding } from './EnginesPanel';
import { DevicesPanel } from './DevicesPanel';
import { FirstRun } from './FirstRun';
import { GuideConnectionDialog, GuideTurnActions, saveGuideConnection, type GuideActions } from './GuideActions';
import { TurnActivity } from './ui/TurnActivity';
import { AgentMessage, ChatComposer, ChatEmptyState, ChatHeader, ChatTranscript, ChatTurn, HumanMessage, TurnStatus } from './ui/ChatPrimitives';
import { isTurnWaitingForInput, liveTurn, livePartial, pendingApprovalSubject, turnPollDelay, type TurnActivityData } from './ui/transcript';
import { TimelineMinimap } from './ui/TimelineMinimap';
import { deriveTimelineMinimapItems } from './ui/timelineMinimapItems';
import { EventConnection } from './EventConnection';
import { AutomationCalendar } from './AutomationCalendar';
import type { AppRoute } from './navigation';
import { applyPresentation, presentationKey, readPresentation } from './presentation';
import { CreateProject, HelpPage, ProjectHome, ProjectSidebar, useProjects } from './Projects';
import { MachinesPanel, RemoteAgentDetail, useMachines } from './MachinesPanel';
import { usePanelFileRequests } from './hooks/usePanelFileRequests';
import { QuestionCard, type Question } from './QuestionCard';
import { ApprovalCard, type AppApproval } from './ApprovalCard';
import { CompactionPanel } from './CompactionPanel';
import { Suspense, useCallback, useEffect, useRef, useState, useId, type FormEvent, type ReactNode } from 'react';
import { Link as LinkIcon, Smartphone, Archive, BarChart3, BookOpen, Cpu, Database, Download, KeyRound, Palette, Plug, SlidersHorizontal, Bell, BellOff, CalendarClock, CalendarDays, Check, ChevronDown, ChevronLeft, ChevronRight, CircleAlert, Clock, Ellipsis, GitFork, ListRestart, Square, Filter, FolderOpen, Hash, Mail, MessageSquare, Monitor, PanelLeft, PanelRight, Paperclip, Pause, Pencil, Play, Plus, RefreshCw, Search, Server, Cable, Trash2, Settings, Workflow as WorkflowIcon, X } from 'lucide-react';
import { Menu, type MenuItem } from './ui/Menu';
import { PageView, useDetailsOpen } from './ui/layout';
import { PanelResizeHandle, sidebarPanel, useResizablePanel } from './ui/resizable';
import { ActionMenu, Alert, Delayed, Badge, Button, Card, Checkbox, Collapsible, ConfirmProvider, Dialog, EmptyState, Field, IconButton, Input, PageHeader, Row, Section, Select, SkeletonRows, Switch, TabPanel, Tabs, Textarea, useConfirm, RecoveryAlert, Tooltip } from './ui/kit';
import { createRoot } from 'react-dom/client';
import { Collapsible as Disclosure, Dialog as RadixDialog, DropdownMenu } from 'radix-ui';
import { validateFlow, type FlowGraph, type FlowLink } from './flows.ts';
import './ui/tokens.css';
import './ui/base.css';
import './ui/shell.css';
import './ui/chat.css';
import './ui/details.css';
import './ui/modals.css';
import './ui/config.css';
import './ui/legacy.css';
import './ui/minarbot.css';
import './ui/kit.css';
import './ui/density.css';
import './Routines.css';
import { RoutineSchedule, scheduleLabel, type RoutineTiming } from './RoutineSchedule';
import { RoutineTemplatePicker, routineTemplates, type RoutineTemplate, type WebhookSource } from './RoutineTemplates';
import '@fontsource-variable/inter';

import { WorkspacePanel, MemoryPanel, WorkflowPanel, WorkflowDirectory, GeneralSettings, PresentationSettings, SharedDataPanel, UsagePanel, BackupPanel, AgentTemplateImport, AgentTemplateExport, McpServerPanel, McpPanel, HubConnection, ConnectedAppsPanel, SkillsPanel, VaultSettings, ComputerSetupPage, panelPrefetchers, settingsPrefetch } from './lazyPanels';
import { usePanelPrefetch } from './hooks/usePanelPrefetch';

import type { Agent } from './AgentProfileDialog';
type Channel = { section_name?: string; project?: string | null; activity: number; read_position: number; revision: number; archived: number; id: string; name: string; instructions: string; members: string[]; lead: string | null };
type Thread = { needs_you?: number; delivery?: boolean; active?: boolean; activity?: number; id: string; agent: string; name: string; revision: number; archived: number; deleted: number; fork?: { source_name: string; source_id: string; source_kind: 'agent' | 'channel' | 'thread'; source_agent: string; cutoff_turn: string; created: string } | null };
type ForkContext = { summary?: string; inherited?: string; turns: { id: string; text: string; reply: string | null }[] };
type AgentMessage = Exchange & { taskId?: string; task?: string; wait?: boolean; generatedFiles?: { id: string; name: string; size: number; thread: string }[]; paused: number; request: string; expects_reply: number; settled: number; id: string; agent: string; thread: string; name: string; status: string; error: string | null; reply: string | null; truncated: boolean };

type Turn = { htmlReplies?: HtmlReplyReference[]; componentReplies?: ComponentReplyData[]; guide?: GuideActions; optimistic?: boolean; history_page?: number; routineReport?: { run: string; name: string; agent: string; thread: string; destination: string; status: string } | null; room?: RoomView | null; harnessApprovals?: HarnessApproval[]; computerWait?: ComputerWait; checkpoint?: { turn: string; restorable: boolean } | null; approvals?: AppApproval[]; recipient?: string | null; dispatch_mode?: 'queue' | 'steer' | 'restart'; dispatch_target?: string | null; dispatch_label?: string | null; activity?: TurnActivityData; editing?: boolean; events?: { id: string; event: string; deadline: number; status: string }[]; receivedAgentReplies?: (AgentMessage & { source: string })[]; messageOrigin?: { kind?: 'peer'; name: string; agent: string | null; channel: string | null; thread: string | null } | null; agentMessages?: AgentMessage[]; partial?: { text: string; model: string; status: string; seq: number } | null; answer_model?: string | null; questions?: Question[]; workflow_run?: string | null; no_update: number; panelFile?: { seq: number; id: string; name: string; openedAt: string; deleted?: number } | null; generatedFiles?: { id: string; name: string; size: number; deleted?: boolean }[]; seq: number; files?: { id: string; name: string; size: number; image?: boolean }[]; revision: number; paused: number; agent: string; parent?: string | null; waiting_for?: string | null; coordination?: { kind: string; child: string | null; prompt: string; owner_name: string; target_name: string }[]; routing?: string | null; agent_name?: string; id: string; text: string; reply: string | null; status: string; error: string | null; created: string };
const mergeHistory = (history: Turn[], page: Turn[]) => [...new Map([...history, ...page].map(turn => [turn.id, turn])).values()].sort((a, b) => a.seq - b.seq);
type Directory = { guide?: Agent | null; hubAvailable?: boolean; agents: Agent[]; configured: boolean; defaultModel: string };
type Environment = { id: string; name: string; origin: string; version: string };
/** One-line sidebar preview: strip Markdown markers so rows read as plain text. */
function plainPreview(text?: string) {
  if (!text) return '';
  return text.replace(/```[\s\S]*?```/g, ' ').replace(/!?\[([^\]]*)\]\([^)]*\)/g, '$1').replace(/[*_~`]+/g, '').replace(/^\s{0,3}(?:#{1,6}|>|[-+*]|\d+[.)])\s+/gm, '').replace(/\s+/g, ' ').trim();
}
type SidebarSummary = { agent: string; preview: string; time: string | null; status: string; needs_you?: number };
type Pending = ChatCommand;
const statusNames: Record<string, string> = { queued: 'Queued', running: 'Replying', done: 'Done', failed: 'Failed', interrupted: 'Interrupted', cancelled: 'Cancelled' };
let environmentIdentity: string | undefined;
async function api<T>(path: string, data?: unknown, signal?: AbortSignal): Promise<T> {
  const isGet = !data;
  try {
    const response = await fetch(path, { ...(!isGet ? { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(data) } : {}), signal })
      .catch(() => { throw new ConnectionError('Minarbot connection lost. Check your connection, then try again.'); });
    const result = await readJson<T>(response);
    if (isGet && path === '/api/environment') {
      const id = (result as Environment).id;
      if (environmentIdentity && environmentIdentity !== id) queryCache.clear();
      environmentIdentity = id;
    }
    return result;
  } finally {
    if (!isGet) queryCache.invalidate();
  }
}
const message = (error: unknown) => error instanceof Error ? error.message : 'Connection failed. Try again.';


function RoomStop({ room, path, archived, onStopped }: { room: RoomView; path: string; archived: boolean; onStopped: () => void }) {
  const key = `minarbot:room-stop:${path}:${room.root}`;
  const [pending, setPending] = useState<{ id: string; generation: string } | null>(() => {
    try { const value = JSON.parse(sessionStorage.getItem(key) ?? 'null'); return value && typeof value.id === 'string' && value.generation === room.generation ? value : null; } catch { return null; }
  });
  const [busy, setBusy] = useState(false), [error, setError] = useState('');
  async function stop() {
    if (busy) return;
    const request = pending ?? { id: crypto.randomUUID(), generation: room.generation };
    try {
      sessionStorage.setItem(key, JSON.stringify(request)); setPending(request); setBusy(true); setError('');
      await api(`${path.replace(/\/turns$/, '')}/discussions/${room.root}/stop`, request);
      sessionStorage.removeItem(key); setPending(null); onStopped();
    } catch (error) { setError(message(error)); }
    finally { setBusy(false); }
  }
  if (!room.active && !pending && !error) return null;
  return <span className="composer-stop"><Tooltip content={busy ? 'Stopping…' : error ? 'Retry Stop' : 'Stop'}><button type="button" className="composer__cta composer__cta--stop" aria-label={busy ? 'Stopping…' : error ? 'Retry Stop' : 'Stop'} disabled={busy || archived} onClick={() => void stop()}><Square size={14} fill="currentColor" aria-hidden /></button></Tooltip>{error && <span className="message-error" role="alert">{error}</span>}</span>;
}

function TaskControls({ turn, path, members, archived, skip = false, onChangeModel }: { onChangeModel?: () => void; skip?: boolean; turn: Turn; path: string; members: Agent[]; archived: boolean }) {
  const [recipient, setRecipient] = useState(''), [confirmed, setConfirmed] = useState(false);
  const [busy, setBusy] = useState(false), [error, setError] = useState('');
  const [resumeOpen, setResumeOpen] = useState(false);
  const pending = useRef<{ action: string; data: { id: string; revision: number; recipient?: string; acknowledgeUncertain?: boolean } } | null>(null);
  if (turn.computerWait || turn.questions?.some(question => question.answer === null) ||
      turn.approvals?.some(approval => approval.state === 'approval' || approval.state === 'ready' && !['failed','interrupted'].includes(turn.status)) ||
      ['done','cancelled'].includes(turn.status)) return null;
  const resumable = !!turn.paused || ['failed','interrupted'].includes(turn.status);
  async function control(action: string) {
    if (busy) return;
    const request = pending.current ?? { action, data: { id: crypto.randomUUID(), revision: turn.revision, ...(action === 'reassign' ? { recipient } : {}), ...(action !== 'pause' ? { acknowledgeUncertain: confirmed } : {}) } };
    pending.current = request; setBusy(true); setError('');
    try { await api(`${path}/${turn.id}/${request.action}`, request.data); pending.current = null; }
    catch (error) { if (error instanceof HttpError && error.status < 500) pending.current = null; setError(message(error)); }
    finally { setBusy(false); }
  }
  return <div className="turn-task-controls bs-root">
    {pending.current ? <IconButton label="Retry control" disabled={busy} onClick={() => void control(pending.current!.action)}><RefreshCw size={14} aria-hidden /></IconButton> : resumable ? <><Button size="sm" variant="ghost" onClick={() => setResumeOpen(true)}>{turn.status === 'failed' ? 'Retry' : 'Resume'}</Button><ActionMenu label="Recovery actions" trigger={<IconButton label="Recovery actions"><Ellipsis size={14} aria-hidden /></IconButton>} items={[...(onChangeModel ? [{ label: 'Change model', onSelect: onChangeModel }] : []), { label: 'Dismiss', onSelect: () => { void control('cancel'); } }]} /><Dialog open={resumeOpen} onOpenChange={setResumeOpen} title="Resume or reassign">
      <div className="k-stack">
        <p className="k-field__hint">Check previous results and effects in the provider. Resuming may resend steps that are not confirmed yet.</p>
        <Checkbox checked={confirmed} onChange={setConfirmed} label="I have checked previous results and effects" />
        <div><Button size="sm" variant="ghost" disabled={busy || archived || !confirmed} onClick={() => void control('resume')}>Resume task</Button></div>
        {members.length > 0 && !turn.approvals?.some(approval => approval.state === 'ready') && <><Field label="New owner"><Select value={recipient} onChange={setRecipient} options={[{ value: '', label: 'Select agent' }, ...members.map(member => ({ value: member.id, label: member.name }))]} /></Field><div><Button size="sm" variant="ghost" disabled={busy || archived || !confirmed || !recipient} onClick={() => void control('reassign')}>Reassign task</Button></div></>}
      </div>
    </Dialog></> : <ActionMenu label={skip ? "Work log actions" : "Task actions"} trigger={skip ? <IconButton label="Work log actions"><Ellipsis size={14} aria-hidden /></IconButton> : <Button size="sm" variant="ghost">Task actions<ChevronDown size={14} aria-hidden /></Button>} items={[{ label: 'Pause', onSelect: () => void control('pause'), disabled: busy || archived }, ...(skip ? [{ label: 'Skip', onSelect: () => void control('cancel'), disabled: busy || archived }] : [])]} />}
    {error && <Alert tone="error">{error}</Alert>}
  </div>;
}

function ForkAnswer({ path, turn, onOpen }: { path: string; turn: Turn; onOpen?: (agent: string, thread: string) => void }) {
  const key = `minarbot:fork:${turn.id}`;
  const [busy, setBusy] = useState(false), [error, setError] = useState('');
  const flight = useRef(false);
  async function fork() {
    if (flight.current) return;
    flight.current = true; setBusy(true); setError('');
    try {
      const saved = sessionStorage.getItem(key), id = saved ?? crypto.randomUUID();
      if (!/^[a-f0-9-]{36}$/i.test(id)) throw new Error('Fork recovery could not be read. Check browser storage before retrying.');
      sessionStorage.setItem(key, id);
      const result = await api<Thread>(`${path}/${turn.id}/fork`, { id });
      sessionStorage.removeItem(key);
      onOpen?.(result.agent, result.id);
    } catch (error) { setError(message(error)); }
    finally { flight.current = false; setBusy(false); }
  }
  return <><IconButton label="Fork from here" disabled={busy || !onOpen} aria-busy={busy || undefined} onClick={() => void fork()}><GitFork size={14} aria-hidden /></IconButton>{error && <span className="message-error" role="alert">{error} Retry uses the same fork request.</span>}</>;
}

function ForkHistory({ context, scope, depth = 0 }: { context: ForkContext; scope: string; depth?: number }) {
  let earlier: ForkContext | null = null;
  if (context.inherited && depth < 8) { try { earlier = JSON.parse(context.inherited); } catch { /* Older nested snapshots may end at the transfer byte limit. */ } }
  return <section className="fork-history" aria-label="Inherited messages">
    {earlier && <ForkHistory context={earlier} scope={scope} depth={depth + 1} />}
    {context.summary && <div className="message-body legacy"><Markdown results text={context.summary} scope={scope} origin={window.location.origin} /></div>}
    {context.turns.map(turn => <div className="turn-block" key={turn.id}><HumanMessage text={turn.text}>{null}</HumanMessage>{turn.reply && <div className="agent-message message-body legacy"><Markdown results text={turn.reply} scope={scope} origin={window.location.origin} /></div>}</div>)}
  </section>;
}

type DetailsContent = { chats: ReactNode; onManageRoutines?: () => void };
function Chat({ agent, configured, firstRun, channel, thread, members = [], onRead, onRestored, onEdit, onOpenAgent, onOpenChannel, menuItems = [], detailsExtra, rename }: { rename?: ReactNode; menuItems?: MenuItem[]; detailsExtra?: DetailsContent; onOpenChannel?: (channel: string) => void; onOpenAgent?: (agent: string, thread: string) => void; onRead?: () => Promise<void>; onRestored?: () => Promise<void>; onEdit?: () => void; agent: Agent | Channel; configured: boolean; firstRun?: boolean; channel?: Channel; thread?: Thread; members?: Agent[] }) {
  const confirm = useConfirm();
  const archived = !!(channel?.archived || thread?.archived || (!channel && (agent as Agent).hidden));
  const scope = thread ? `thread:${thread.id}` : channel ? `channel:${agent.id}` : agent.id;
  const path = thread ? `/api/threads/${thread.id}/turns` : `/api/${channel ? 'channels' : 'agents'}/${agent.id}/turns`;
  const [showMemory, setShowMemory] = useState(false);
  const nextFrame = useNextFrame();
  const [focusedCard, setFocusedCard] = useState<string>();
  const [focusedTask, setFocusedTask] = useState(() => { try { const key = `minarbot:focus:${scope}`, id = sessionStorage.getItem(key) ?? ''; sessionStorage.removeItem(key); return /^[a-f0-9-]{36}$/i.test(id) ? id : ''; } catch { return ''; } });
  const [query, setQuery] = useState('');
  const [finding, setFinding] = useState(false);
  const [historyPages, setHistoryPages] = useState<number[]>([]);
  const before = historyPages.at(-1);
  const historyTop = useRef<HTMLDivElement>(null);
  const focusHistory = useRef(false);
  const focusedOnce = useRef('');
  const loadedTask = useRef<string | null>(null);
  const findHistory = useRef<Turn[] | null>(null);
  const inheritedLoaded = useRef(false);
  const [pastedFiles, setPastedFiles] = useState<File[]>([]);
  const attachmentKey = `minarbot:attachments:${scope}`;
  const [attached, setAttached] = useState<string[]>(() => { try { const ids = JSON.parse(sessionStorage.getItem(attachmentKey) ?? '[]'); return Array.isArray(ids) && ids.length <= 4 && ids.every(id => typeof id === 'string') ? ids : []; } catch { return []; } });
  function attach(ids: string[]) { try { sessionStorage.setItem(attachmentKey, JSON.stringify(ids)); setAttached(ids); } catch { setError('Attachment selection was not saved. Check browser storage.'); } }
  const storageKey = `minarbot:pending:${scope}`;
  const [turns, setTurns] = useState<Turn[] | null>(null);
  const [inheritedContext, setInheritedContext] = useState<ForkContext | null>(null);
  const optimistic = useOptimisticTurns<Turn>();
  const [queueTurns, setQueueTurns] = useState<Turn[]>([]);
  const [activeTurns, setActiveTurns] = useState<{ id: string; agent: string; capabilities: { steer: boolean; queue: boolean; restart: boolean } }[]>([]);
  const [addressedTurn, setAddressedTurn] = useState('');
  const dispatchTarget = activeTurns.find(turn => channel ? turn.id === addressedTurn : true);
  const targets = channel ? members.filter(member => !dispatchTarget || member.id === dispatchTarget.agent) : [agent as Agent];
  const blockedAgent = targets.find(target => target.runnable === false);
  const canSend = !blockedAgent && (targets.length ? targets.every(target => target.runnable ?? configured) : false);
  const needsEngineLogin = blockedAgent?.provider_available === false && isLoginBinding(blockedAgent.provider_account ?? '');
  const needsProvider = blockedAgent?.provider_available === false || !blockedAgent && targets.length > 0 && !canSend;
  const firstMessage = firstRun && !thread && !turns?.length;
  const draftKey = `minarbot:draft:${scope}`;
  const [draft, setDraft] = useState(() => { try { return sessionStorage.getItem(draftKey) ?? ''; } catch { return ''; } });
  const [recovery] = useState(() => {
    try { return { command: restoreChatCommand(sessionStorage.getItem(storageKey), !!channel), blocked: false }; }
    catch { return { command: null, blocked: true }; }
  });
  const [pending, setPending] = useState<Pending | null>(recovery.command), [blocked, setBlocked] = useState(recovery.blocked);
  const composer = useRef<HTMLTextAreaElement>(null);
  const [htmlContext, setHtmlContext] = useState<HtmlReplyContext | undefined>(recovery.command?.htmlContext);
  const canSteer = !!dispatchTarget?.capabilities.steer && !htmlContext;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [readError, setReadError] = useState('');
  const bottom = useRef<HTMLDivElement>(null);
  const { transcript, nearBottom, onScroll, jumpToLatest, showJump } = useTimelineFollow(bottom, !focusedTask && !before && !finding);
  const mergeStream = useTurnStream<Turn>(path, setTurns, !focusedTask && !before && !finding);
  usePolling(async () => {
    if (turns && nearBottom.current && !document.querySelector('[role="dialog"], [role="alertdialog"]')) {
      try { await onRead?.(); } catch (error) { setError(message(error)); }
    }
  }, { interval: 1000, enabled: !!channel && channel.activity > channel.read_position && !focusedTask && !before && !finding }, [channel?.id, channel?.activity, channel?.read_position]);
  const requestActive = useRef(false);
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  useEffect(() => { loadedTask.current = null; inheritedLoaded.current = false; if (!finding && !findHistory.current) setTurns(null); }, [path, focusedTask, before, finding]);
  usePolling(async signal => {
    let delay = 1200;
    try {
      const [active, page, latest] = await Promise.all([api<typeof activeTurns>(path.replace(/\/turns$/, '/dispatch'), undefined, signal), api<Turn[]>(focusedTask ? `${path}?focus=${focusedTask}` : `${path}?${new URLSearchParams({ ...(before ? { before: String(before) } : {}) })}`, undefined, signal), !channel && (focusedTask || before) ? api<Turn[]>(path, undefined, signal) : null]);
      if (!inheritedLoaded.current && thread?.fork) {
        const context = await api<ForkContext>(`/api/threads/${thread.id}/fork-context`, undefined, signal);
        if (signal.aborted) return;
        setInheritedContext(context); inheritedLoaded.current = true;
      }
      if (signal.aborted) return;
      let data = page;
      if (finding) {
        if (findHistory.current) data = mergeHistory(findHistory.current, page);
        else {
          let older = page.filter(turn => turn.history_page !== 0);
          while (!signal.aborted && older.length >= 100) {
            older = await api<Turn[]>(`${path}?before=${older[0].seq}`, undefined, signal);
            data = mergeHistory(older, data);
          }
        }
        if (signal.aborted) return;
        findHistory.current = data;
      } else if (findHistory.current && !focusedTask && !before) {
        data = mergeHistory(findHistory.current, page);
        findHistory.current = data;
      }
      if (!signal.aborted) { setActiveTurns(active); setQueueTurns(optimistic.merge(latest ?? page)); loadedTask.current = focusedTask; setTurns(optimistic.merge(mergeStream(data))); setReadError(''); delay = turnPollDelay(latest ?? page); }
    }
    catch (error) { if (!signal.aborted) setReadError(message(error)); }
    return delay;
  }, { interval: 1200 }, [path, focusedTask, before, finding]);
  useEffect(() => {
    if (focusedTask && loadedTask.current === focusedTask && turns) {
      const key = focusedCard ?? focusedTask;
      const target = (focusedCard && document.getElementById(focusedCard)) || document.getElementById(`turn-${focusedTask}`);
      if (target && focusedOnce.current !== key) {
        focusNeedsYouTarget(target);
        focusedOnce.current = key;
      }
    }
    if (!focusedTask && loadedTask.current === '' && turns && focusHistory.current) {
      const target = !finding && !before && turns.length ? document.getElementById(`turn-${turns.at(-1)!.id}`) : historyTop.current;
      if (target) { target.focus({ preventScroll: true }); target.scrollIntoView({ block: before ? 'start' : 'end' }); focusHistory.current = false; }
    }
    if (!focusedTask && !before && !finding && nearBottom.current) jumpToLatest();
  }, [focusedTask, focusedCard, turns, before, finding]);
  useEffect(() => {
    if (!requestActive.current && pending && turns?.some(turn => turn.id === pending.id && !turn.optimistic)) {
      try { sessionStorage.removeItem(storageKey); sessionStorage.removeItem(draftKey); sessionStorage.removeItem(attachmentKey); } catch { /* The same ID remains safe to retry. */ }
      setPending(null); setDraft(''); setAttached([]); setHtmlContext(undefined); setError('');
    }
  }, [turns, pending, storageKey, draftKey, attachmentKey]);
  function htmlReplyAction(action: HtmlReplyAction) {
    if (busy || pending || blocked || archived) return;
    if (action.kind === 'context') {
      setHtmlContext(action.context);
      return;
    }
    const value = draft.trim() ? `${draft}\n\n${action.text}` : action.text;
    if (value.length > 16000) {
      setError('Free up space in your draft before adding this reply.');
      return;
    }
    setDraft(value);
    try { sessionStorage.setItem(draftKey, value); }
    catch { setError('Draft was not saved. Copy your message before leaving.'); }
    composer.current?.focus();
  }
  async function send(event: FormEvent, forcedMode?: 'queue' | 'steer') {
    event.preventDefault();
    if (requestActive.current || pastedFiles.length || attachments.uploading || blocked || (!pending && (!canSend || !draft.trim() && !attached.length))) return;
    if (archived) { setError('Restore the conversation before sending a new message.'); return; }
    const mode = dispatchTarget && !htmlContext ? forcedMode ?? 'queue' : 'queue';
    const request = pending ?? { id: crypto.randomUUID(), text: draft.trim(), ...(mode !== 'queue' ? { dispatchMode: mode, targetTurn: dispatchTarget!.id, ...(channel ? { recipient: dispatchTarget!.agent } : {}) } : {}), ...(channel && mode === 'queue' && dispatchTarget ? { recipient: dispatchTarget.agent } : {}), ...(attached.length ? { files: attached } : {}), ...(htmlContext ? { htmlContext } : {}) };
    try { sessionStorage.setItem(storageKey, JSON.stringify(request)); }
    catch { setError('The browser could not save send status. Enable site storage before sending.'); return; }
    requestActive.current = true; setBusy(true); setPending(request); setError(''); nearBottom.current = true;
    const local: Turn = { id: request.id, text: request.text, seq: Math.max(0, ...(turns ?? []).map(turn => turn.seq), ...queueTurns.map(turn => turn.seq)) + 1, agent: agent.id, status: dispatchTarget || turns?.some(turn => turn.optimistic && turn.status === 'running') ? 'queued' : 'running', optimistic: true, revision: 1, paused: 0, no_update: 0, reply: null, error: null, created: new Date().toISOString(), files: attachments.selectedFiles.filter(file => request.files?.includes(file.id)) };
    optimistic.add(local); setTurns(rows => optimistic.merge(rows ?? [], false)); setQueueTurns(rows => optimistic.merge(rows, false));
    setFocusedTask(''); setHistoryPages([]); setQuery(''); setFinding(false);
    try {
      await api<Turn>(path, request);
      // A remounted conversation owns recovery and any newer draft after navigation.
      if (!mounted.current) return;
      setAddressedTurn('');
      sessionStorage.removeItem(storageKey); sessionStorage.removeItem(draftKey); sessionStorage.removeItem(attachmentKey); setPending(null); setDraft(''); setAttached([]); setHtmlContext(undefined); setFocusedTask(''); setHistoryPages([]); setQuery(''); setFinding(false);
    } catch (error) {
      if (!mounted.current) return;
      if (error instanceof HttpError && (error.status < 500 || error.status === 503 && !error.restarting)) {
        try { sessionStorage.removeItem(storageKey); sessionStorage.setItem(draftKey, request.text); } catch { /* Keep the draft in memory if storage fails. */ }
        optimistic.remove(request.id); setTurns(rows => rows?.filter(turn => turn.id !== request.id) ?? null); setQueueTurns(rows => rows.filter(turn => turn.id !== request.id));
        setPending(null); setDraft(request.text);
      }
      setError(message(error));
    }
    finally { requestActive.current = false; setBusy(false); }
  }
  async function cancel(turn: Turn) {
    if (!await confirm({ title: 'Stop this task?', description: 'Steps already sent may have taken effect.', confirm: 'Stop task', danger: true })) return;
    try { await api(`${path}/${turn.id}/cancel`, { id: crypto.randomUUID() }); setError(''); }
    catch (error) { setError(message(error)); }
  }
  const [menuOpen, setMenuOpen] = useState(false), [summaryOpen, setSummaryOpen] = useState(false);
  const [queueOrderOpen, setQueueOrderOpen] = useState(false);
  const [conversationSettingsOpen, setConversationSettingsOpen] = useState(false);
  const [detailsOpen, setDetailsOpen] = useDetailsOpen();
  const rightPanel = useRightPanel(scope);
  const inspectRun = (target: RunTarget) => { rightPanel.openRun(target); setDetailsOpen(true); };
  useEffect(() => {
    const key = (event: KeyboardEvent) => {
      if (!document.querySelector('[role="dialog"], [role="alertdialog"], [role="menu"]')) {
        if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'f') {
          event.preventDefault(); nearBottom.current = false; setFinding(true); setFocusedTask(''); setHistoryPages([]);
          nextFrame(() => document.querySelector<HTMLInputElement>('.message-find input')?.select()); return;
        }
        if (finding && event.key === 'Escape') { event.preventDefault(); setFinding(false); setQuery(''); composer.current?.focus(); return; }
      }
      if (event.key === 'F6' && !document.querySelector('[role="dialog"], [role="alertdialog"], [role="menu"]')) { event.preventDefault(); composer.current?.focus(); return; }
      if ((event.metaKey || event.ctrlKey) && event.shiftKey && event.key.toLowerCase() === 'b' && !document.querySelector('[role="dialog"], [role="alertdialog"], [role="menu"]')) {
        event.preventDefault(); event.stopImmediatePropagation(); setDetailsOpen(!detailsOpen);
      }
    };
    window.addEventListener('keydown', key, true);
    return () => window.removeEventListener('keydown', key, true);
  }, [detailsOpen, setDetailsOpen, finding]);
  const attachments = useConversationAttachments({ path: path.replace(/\/turns$/, '/files'), archived, attached: pending?.files ?? attached, onAttach: attach, locked: blocked || !!pending || busy, pastedFiles, onPasteSettled: () => setPastedFiles([]), onFiles: setPastedFiles, filesOpen: detailsOpen && rightPanel.tab === 'files', api });
  const openOutput = useCallback((file: { id: string; name: string }) => { rightPanel.selectFile({ name: file.name, path: `${path.replace(/\/turns$/, '')}/panel/files/${file.id}` }); setDetailsOpen(true); }, [rightPanel.selectFile, path, setDetailsOpen]);
  usePanelFileRequests(turns, openOutput);
  const openFocused = (id: string, card?: string) => { findHistory.current = null; focusedOnce.current = ''; setQuery(''); setFinding(false); setFocusedTask(id); setFocusedCard(card); };
  useNeedsYouFocus(scope, (turn, card) => {
    setHistoryPages([]);
    openFocused(turn, card);
  });
  if (showMemory) return <PageView><Suspense fallback={<Delayed><p className="empty" role="status">Loading memory...</p></Delayed>}><MemoryPanel key={scope} path={`/api/${channel ? 'channels' : 'agents'}/${agent.id}/memory`} channel={!!channel} archived={!!channel?.archived} request={api} onClose={() => { setShowMemory(false); nextFrame(() => document.querySelector<HTMLButtonElement>('.chat__pill')?.focus()); }} /></Suspense></PageView>;
  const refreshTurns = () => { window.dispatchEvent(new Event('minarbot:message-changed')); void Promise.all([api<Turn[]>(path), api<typeof activeTurns>(path.replace(/\/turns$/, '/dispatch'))]).then(([data, active]) => { setQueueTurns(optimistic.merge(data)); if (!focusedTask && !before && !finding) setTurns(optimistic.merge(mergeStream(findHistory.current ? mergeHistory(findHistory.current, data) : data))); setActiveTurns(active); }).catch(error => setError(message(error))); };
  const queued = (turn: Turn) => !channel && activeTurns.some(active => active.id !== turn.id) && turn.status === 'queued' && !turn.paused && !isTurnWaitingForInput(turn) && !turn.events?.some(event => event.status === 'waiting') && !turn.parent && !turn.waiting_for && !turn.routineReport && !turn.workflow_run && !turn.activity?.calls.length && !turn.agentMessages?.length;
  const runningTurn = !channel ? activeTurns[0] : undefined;
  const runningRoom = channel ? turns?.find(turn => turn.room?.active)?.room : undefined;
  const title = thread?.name ?? agent.name;
  const builtin = !channel && (agent as Agent).builtin;
  const editLabel = builtin ? 'AI connection' : channel ? 'Section settings' : thread ? 'Conversation settings' : 'Edit profile';
  const items: MenuItem[] = [
    ...menuItems.filter(item => !['Export template', 'Delete'].includes(item.label)),
    ...(thread ? [{ label: 'Conversation settings', icon: <SlidersHorizontal size={15} aria-hidden />, onSelect: () => setConversationSettingsOpen(true) }] : onEdit ? [{ label: editLabel, icon: <Pencil size={15} aria-hidden />, onSelect: onEdit }] : []),
    ...(channel ? [{ label: `${channel ? 'Section' : 'Agent'} memory`, prefetch: MemoryPanel.prefetch, icon: <BookOpen size={15} aria-hidden />, onSelect: () => setShowMemory(true) }, { label: 'History summary', icon: <ListRestart size={15} aria-hidden />, onSelect: () => setSummaryOpen(true) }] : []),
    { label: detailsOpen ? 'Hide panel' : 'Show panel', icon: <PanelRight size={15} aria-hidden />, onSelect: () => setDetailsOpen(!detailsOpen) },
    ...menuItems.filter(item => item.label === 'Export template').map(item => ({ ...item, separator: true })),
    ...menuItems.filter(item => item.label === 'Delete'),
  ];
  const statusLabel = (turn: Turn) => turn.computerWait ? 'Waiting for you' : pendingApprovalSubject(turn) && !['done','cancelled'].includes(turn.status) ? pendingApprovalSubject(turn) === 'form' ? 'Waiting for your input' : 'Waiting for approval' : turn.editing ? 'Editing' : turn.questions?.some(question => question.answer === null) && !['done','cancelled'].includes(turn.status) ? 'Waiting for your answer' : turn.no_update ? 'No updates' : turn.paused && !['done','cancelled'].includes(turn.status) ? 'Paused' : turn.agentMessages?.some(item => item.expects_reply && !item.settled) && turn.status === 'queued' ? 'Waiting for agent reply' : turn.waiting_for && turn.status === 'queued' ? 'Waiting for delegation' : turn.routing === 'pending' && turn.status === 'running' ? 'Selecting recipient' : turn.routing === 'clarify' ? 'Needs clarification' : turn.routing === 'none' ? 'No task needed' : turn.status === 'running' ? 'Running' : statusNames[turn.status] ?? turn.status;
  const tone = (turn: Turn) => pendingApprovalSubject(turn) === 'form' && turn.status === 'running' || turn.questions?.some(question => question.answer === null) && !['done','cancelled'].includes(turn.status) || turn.paused && !['done','cancelled'].includes(turn.status) ? 'attention' : ['failed','interrupted'].includes(turn.status) ? 'error' : ['queued','running'].includes(turn.status) ? 'working' : 'done';
  const time = (value: string) => new Date(value).toLocaleTimeString('en-US', { hour: '2-digit', minute: '2-digit' });
  const turnRequests = (turn: Turn) => {
    const pathToTurn = `${path}/${turn.id}`;
    const card = (kind: PendingInput['kind'], request: string, content: ReactNode) => {
      const id = requestKey({ kind, request, turn: turn.id });
      return <div key={id} id={id} tabIndex={-1} className="turn-request">{content}</div>;
    };
    return <>
      {turn.harnessApprovals?.map(approval => card('harness', approval.id, approval.kind === 'form'
        ? !!approval.input && <ComponentFormCard reply={approval.input as ComponentReplyData} revision={approval.revision} state={approval.state} decision={approval.decision}
            path={pathToTurn} scope={path.slice(0, -6)} disabled={archived || turn.status !== 'running'} request={api} />
        : <HarnessApprovalCard approval={approval} path={pathToTurn} status={turn.status} archived={archived} request={api} />))}
      {turn.approvals?.map(approval => card('app', approval.id, <ApprovalCard approval={approval} path={pathToTurn} status={turn.status} archived={archived} request={api} />))}
      {turn.computerWait && card('computer', turn.computerWait.call_id, <ComputerRequestCard agent={turn.agent} name={turn.agent_name ?? agent.name} wait={turn.computerWait} request={api} />)}
      {turn.guide && <GuideTurnActions actions={turn.guide} request={api} disabled={!!(agent as Agent).hidden} onChanged={refreshTurns} />}
      {turn.questions?.map(question => card('question', question.id, <QuestionCard question={question} path={pathToTurn} status={turn.status} archived={archived} request={api} />))}
    </>;
  };
  const answerActions = (turn: Turn) => {
    if (!['done', 'cancelled', 'failed', 'interrupted'].includes(turn.status)) return null;
    const text = turn.reply || turn.partial?.text || turn.activity?.stream?.filter(segment => segment.kind === 'text').map(segment => segment.text).join('\n\n') || turn.error || statusNames[turn.status];
    const finished = turn.activity?.calls.at(-1)?.finished ?? turn.created;
    return <><MessageCopyButton text={text} /><ForkAnswer path={path} turn={turn} onOpen={onOpenAgent} /><time className="message-time" dateTime={finished} title={new Date(finished).toLocaleString('en-US')}>{time(finished)}</time></>;
  };
  const waiting = turns?.flatMap(turn => turn.events?.filter(event => event.status === 'waiting').map(event => ({ turn, event })) ?? []) ?? [];
  return <div className="chat-row bs-root">
    {queueOrderOpen && <Dialog open title="Queue order" onOpenChange={setQueueOrderOpen}><QueuePanel path={path} request={api} onOpen={id => { setQueueOrderOpen(false); openFocused(id); }} /></Dialog>}
    {conversationSettingsOpen && <Dialog open title="Conversation settings" onOpenChange={setConversationSettingsOpen}><div className="k-stack"><Button icon={<BookOpen size={16} aria-hidden />} onPointerEnter={MemoryPanel.prefetch} onFocus={MemoryPanel.prefetch} onClick={() => { setConversationSettingsOpen(false); setShowMemory(true); }}>Agent memory</Button><Button icon={<ListRestart size={16} aria-hidden />} onClick={() => { setConversationSettingsOpen(false); setSummaryOpen(true); }}>History summary</Button></div></Dialog>}
    {summaryOpen && <Dialog open title="Conversation context" onOpenChange={setSummaryOpen}><CompactionPanel path={path.replace(/\/turns$/, '/compaction')} request={api} /></Dialog>}
    <main className={`chat${finding ? ' is-finding' : ''}`} aria-label={`Conversation ${title}`}>
      {finding && <MessageFind query={query} onQuery={setQuery} viewport={transcript} content={turns} onClose={() => { setFinding(false); setQuery(''); composer.current?.focus(); }} />}
      <ChatTranscript ref={transcript} onScroll={onScroll} minimap={<TimelineMinimap key={`${path}:${before}:${finding}:${focusedTask}`} items={deriveTimelineMinimapItems(turns ?? [])} viewport={transcript} onNavigate={() => { nearBottom.current = false; }} />} jump={showJump && !focusedTask && !before && !finding ? <IconButton className="jump-btn" label="Jump to latest" onClick={jumpToLatest}><ChevronDown size={16} aria-hidden /></IconButton> : null}>
            {(focusedTask || before) && <div className="history-banner" role="status"><span>{focusedTask ? channel ? 'Task family' : 'Selected message' : 'Older history'}</span><button type="button" className="link-btn" onClick={() => { focusHistory.current = true; setTurns(null); setFocusedTask(''); setHistoryPages([]); setQuery(''); setFinding(false); focusedOnce.current = ''; nearBottom.current = true; }}>Back to latest messages</button></div>}
            {!focusedTask && <div ref={historyTop} tabIndex={-1} className="history-navigation" aria-label="History navigation">
              {!finding && !findHistory.current && !!turns && turns.filter(turn => turn.history_page !== 0).length >= 100 && <button type="button" className="link-btn" onClick={() => { if (turns?.length) { focusHistory.current = true; setTurns(null); setHistoryPages([...historyPages, turns.find(turn => turn.history_page !== 0)!.seq]); } }}>Older messages</button>}
              {!!before && <button type="button" className="link-btn" onClick={() => { focusHistory.current = true; setTurns(null); setHistoryPages(historyPages.slice(0, -1)); nearBottom.current = true; }}>Newer messages</button>}
            </div>}
            {inheritedContext && <ForkHistory context={inheritedContext} scope={path.slice(0, -6)} />}
            {thread?.fork && <div className="fork-origin" aria-label="Fork origin" title={new Date(thread.fork.created).toLocaleString('en-US')}><span>Forked from <a className="link-btn" href={`#turn-${thread.fork.cutoff_turn}`} onClick={event => { event.preventDefault(); const origin = thread.fork!; try { sessionStorage.setItem(`minarbot:focus:${origin.source_kind === 'channel' ? `channel:${origin.source_id}` : origin.source_kind === 'thread' ? `thread:${origin.source_id}` : origin.source_agent}`, origin.cutoff_turn); } catch { /* Conversation link remains usable without focus recovery. */ } if (origin.source_kind === 'channel') onOpenChannel?.(origin.source_id); else onOpenAgent?.(origin.source_agent, origin.source_kind === 'thread' ? origin.source_id : ''); }}>{thread.fork.source_name}</a></span></div>}
            {readError && <p role="alert" className="chat-notice is-error">{readError} History will be retried.</p>}
            {turns === null ? <p className="empty-chat__sub" role="status">Loading conversation...</p>
              : turns.length === 0 && before ? <p className="empty-chat__sub" role="status">Start of history reached. Select Newer messages to return.</p>
              : turns.length === 0 && thread?.fork ? null
              : turns.length === 0 ? <ChatEmptyState agent={agent} title={thread?.fork ? `Continue with ${agent.name}` : undefined}>{builtin ? 'What work do you want to set up?' : 'role' in agent ? agent.role : null}</ChatEmptyState>
              : turns.filter(turn => !queued(turn) && !(turn.status === 'cancelled' && !turn.error?.startsWith('Stopped') && !turn.activity?.calls.length && !turn.room) && (!turn.room || turn.room.root === turn.id || !turns.some(root => root.id === turn.room!.root))).map(turn => {
                const approvalSubject = pendingApprovalSubject(turn);
                return <ChatTurn key={turn.id} id={`turn-${turn.id}`} className={`turn-block${turn.id === turns.at(-1)?.id ? ' is-latest' : ''}`}>
                {turn.routineReport ? <section className="routine-report" aria-label={`Routine report: ${turn.routineReport.name}`}><div className="routine-report__meta"><CalendarClock size={16} aria-hidden /><strong>Routine report</strong><time dateTime={turn.created}>{time(turn.created)}</time></div><div className="legacy routine-report__body"><Markdown results files={turn.generatedFiles} text={turn.reply ?? ''} scope={path.slice(0, -6)} origin={window.location.origin} /></div><a className="k-link" href={`#routine-run-${turn.routineReport.run}`} onClick={event=>{event.preventDefault();onOpenAgent?.(turn.routineReport!.agent,turn.routineReport!.thread);}}>Open full run</a></section> : <><HumanMessage text={turn.text} label={turn.messageOrigin?.kind === 'peer' ? `Message from ${turn.messageOrigin.name}` : 'Your message'} attachments={!!turn.files?.length && <Attachments files={turn.files} path={path.replace(/\/turns$/, '/files')} />}>
                    <time className="message-time" dateTime={turn.created} title={new Date(turn.created).toLocaleString('en-US')}>{time(turn.created)}</time>
                    {turn.messageOrigin?.kind === 'peer' ? <span className="message-origin">From {turn.messageOrigin.name}</span> : turn.workflow_run ? <span className="message-origin">Workflow</span> : turn.parent ? <span className="message-origin">Subtask</span> : null}
                    <MessageCopyButton text={turn.text} label="Copy message" />
                    {turn.parent && !turn.messageOrigin && <a className="link-btn link-btn--small" href={`#turn-${turn.parent}`} onClick={event => { event.preventDefault(); openFocused(turn.parent!); }}>View parent task</a>}
                    {turn.messageOrigin && turn.messageOrigin.kind !== 'peer' && <button type="button" className="link-btn link-btn--small" onClick={() => turn.messageOrigin!.channel ? onOpenChannel?.(turn.messageOrigin!.channel) : onOpenAgent?.(turn.messageOrigin!.agent!, turn.messageOrigin!.thread ?? '')}>Open requester conversation</button>}
                </HumanMessage>
                {turn.room ? <><RoomMessages onHtmlReplyAction={htmlReplyAction} room={turn.room} members={members} scope={path.slice(0, -6)} turns={turns} controls={id => { const item = turns.find(item => item.id === id); return item && <><TaskControls key={`${item.id}:${item.revision}`} turn={item} path={path} members={members} archived={archived} skip />{answerActions(item)}</>; }} pending={turns.filter(item => item.room?.root === turn.room!.root && !item.room.silent && !turn.room!.messages.some(message => message.kind === 'reply' && message.turn === item.id))} details={id => { const item = turns.find(item => item.id === id); return item && <TurnActivity scope={path.slice(0, -6)} logOnly turn={item} agentName={members.find(member => member.id === item.agent)?.name ?? agent.name} onOpenTask={openFocused} onOpenAgent={onOpenAgent} onInspect={inspectRun} />; }} extras={id => { const item = turns.find(item => item.id === id); return item && turnRequests(item); }} /></> : <AgentMessage identity={channel && members.length > 1 && <div className="agent-identity"><AgentAvatar agent={members.find(member => member.id === turn.agent) ?? { id: turn.agent, name: turn.agent_name ?? agent.name }} size={24} /><strong className="message-name">{turn.agent_name ?? agent.name}</strong></div>}>
                    <TurnActivity scope={path.slice(0, -6)} turn={turn} agentName={agent.name} onOpenTask={openFocused} onOpenAgent={onOpenAgent} onInspect={inspectRun} />
                    {!channel && (!!turn.agentMessages?.length || !!turn.receivedAgentReplies?.length) && <AgentConversation items={[...(turn.agentMessages ?? []), ...(turn.receivedAgentReplies ?? []).filter(item => !turn.agentMessages?.some(sent => sent.id === item.id))]} sender={turn.agent_name ?? agent.name} scope={path.slice(0, -6)} onOpen={onOpenAgent} />}
                    <div className="message-body">
                      <HtmlReplies replies={turn.htmlReplies} scope={path.slice(0, -6)} onAction={htmlReplyAction} />
                      <ComponentReplies replies={turn.componentReplies} scope={path.slice(0, -6)} />
                      {channel && turn.parent && <div className="room-request"><strong>{turn.messageOrigin?.name ?? turn.agent_name ?? agent.name}</strong><p>{turn.text}</p></div>}
                      {turn.dispatch_mode === 'steer' && turn.dispatch_label && ['queued', 'running'].includes(turn.status) && <p className="dispatch-receipt">{turn.dispatch_label}{turn.dispatch_target && <> · <button type="button" className="link-btn link-btn--small" onClick={() => openFocused(turn.dispatch_target!)}>View target turn</button></>}</p>}
                      {turn.status === 'cancelled' && !turn.reply && !turn.partial?.text ? null : turn.status === 'done' && !turn.reply && (turn.dispatch_label || turn.htmlReplies?.length || turn.componentReplies?.length || turn.harnessApprovals?.some(approval => approval.kind === 'form')) ? null : (turn.reply || livePartial(turn) || turn.status === 'cancelled' && turn.partial?.text) ? <div className={livePartial(turn) ? "live-answer" : "reply-answer"} aria-label={livePartial(turn) ? "Streaming answer" : "Agent reply"}><div className="legacy"><StreamingMarkdown files={turn.generatedFiles} key={turn.id} text={turn.reply ?? turn.partial?.text ?? ''} scope={path.slice(0, -6)} origin={window.location.origin} /></div>{livePartial(turn) && <span className="stream-caret" aria-hidden />}</div>
                        : approvalSubject === 'form' && !turn.error ? null : <p className={turn.error ? 'message-error' : 'message-muted'}>{turn.error && <CircleAlert size={14} aria-hidden />}{turn.error ?? <span>{approvalSubject ? `Review the ${approvalSubject} to continue.` : turn.questions?.some(question => question.answer === null) ? 'Answer the question to continue the task.' : turn.workflow_run ? 'Workflow finished without a text result.' : turn.agentMessages?.some(item => item.expects_reply && !item.settled) ? 'Results will be combined after all recipients finish. New messages include results already available.' : turn.events?.some(event => event.status === 'waiting') ? 'Waiting for a signed event.' : turn.waiting_for ? 'Waiting for child task results.' : turn.status === 'queued' ? 'Waiting for turn.' : null}</span>}</p>}
                      {!turn.reply && !turn.partial?.text && !['queued', 'running'].includes(turn.status) && !!turn.generatedFiles?.length && <div className="legacy"><Markdown results text="" files={turn.generatedFiles} scope={path.slice(0, -6)} origin={window.location.origin} /></div>}
                      {!turn.reply && turn.partial?.text && !livePartial(turn) && turn.status !== 'cancelled' && <div className="message-partial"><span className="message-muted">Partial reply</span><div className="legacy"><Markdown results files={turn.generatedFiles} text={turn.partial.text} scope={path.slice(0, -6)} origin={window.location.origin} /></div></div>}
                    </div>
                    {(!['failed','cancelled'].includes(turn.status) && ((turn.status !== 'done' && !['Running', 'Queued'].includes(statusLabel(turn))) || turn.activity?.fallback_model)) && <TurnStatus tone={tone(turn)} label={turn.activity?.fallback_model ? `${statusLabel(turn)} · Fallback model used` : statusLabel(turn)} />}
                    <div className="turn-extras k-stack">
                      {turn.events?.map(event => <section className="k-stack" key={event.id}><Row title={`Event ${event.event}: ${event.status === 'waiting' ? 'Waiting' : event.status === 'received' ? 'Received' : event.status === 'expired' ? 'Expired' : 'Closed'}`} meta={new Date(event.deadline).toLocaleString('en-US')} />{event.status === 'waiting' && !archived && <EventConnection path={`/api/turn-waits/${event.id}`} request={api} />}</section>)}
                      {turnRequests(turn)}
                      {!!turn.coordination?.length && <Collapsible title={`Task coordination (${turn.coordination.length})`}>{turn.coordination.map((action, index) => <div className="k-stack" key={index}><div className="k-section__title">{action.owner_name} {action.kind === 'delegate_task' ? 'delegated to' : 'reassigned to'} {action.target_name}.</div><div className="message-plain">{action.prompt}</div>{action.child && <div className="k-row-actions"><Button size="sm" variant="ghost" onClick={() => openFocused(action.child!)}>View subtask</Button></div>}</div>)}</Collapsible>}
                    </div>
                    <div className="turn-controls" role="group" aria-label="Message actions">
                      {turn.status === 'queued' && !turn.approvals?.length && turn.dispatch_mode !== 'steer' && !isTurnWaitingForInput(turn) && <QueueEditor turn={{ ...turn, files: turn.files ?? [] }} path={path} archived={archived} request={api} onChanged={refreshTurns} />}
                      {!channel && turn.checkpoint?.restorable && ['done', 'failed', 'cancelled', 'interrupted'].includes(turn.status) && <RestoreCheckpoint agent={turn.agent} turn={turn.id} request={api} />}
                      {answerActions(turn)}
                      {turn.reply && <SpeechPlayback key={`${path}:${turn.id}`} owner={`${path}:${turn.id}`} text={turn.reply} />}
                      {turn.dispatch_mode !== 'steer' && !['done','cancelled'].includes(turn.status) && <TaskControls key={`${turn.id}:${turn.revision}`} turn={turn} path={path} members={channel ? members : []} archived={archived || !!turn.editing} onChangeModel={onEdit} />}
                      {turn.status === 'queued' && !queued(turn) && <Button size="sm" variant="ghost" onClick={() => void cancel(turn)}>{turn.waiting_for ? 'Cancel task and delegation' : turn.paused || isTurnWaitingForInput(turn) ? 'Cancel task' : 'Cancel queued task'}</Button>}
                    </div>
                </AgentMessage>}</>}
              </ChatTurn>;
              })}
            <div ref={bottom} />
      </ChatTranscript>
      <footer className="composer-area">
        {!channel && (agent as Agent).hidden ? <HiddenAgentBanner agent={agent as Agent} request={api} onChanged={onRestored} /> : archived && <p className="composer__note">Archived</p>}
        {!canSend && !archived && <p className="composer__note composer__note--blocked">
          {builtin && onEdit ? <Button size="sm" onClick={onEdit}>Choose AI connection</Button> : needsProvider && <a className="k-link" href={routePath({ ...parseRoute(window.location.pathname + window.location.search), settings: needsEngineLogin ? 'engines' : 'providers' })}>{needsEngineLogin ? 'Check engine login' : 'Connect a provider'}</a>}
          {!builtin && blockedAgent?.blocked_reason && <span>{blockedAgent.blocked_reason}</span>}
          {!targets.length && 'No available members.'}
        </p>}
        {firstMessage && canSend && <p className="composer__note">Send first message</p>}
        {error && <div className="composer__error" role="alert"><span>{error} {pending && 'Use Retry send to check the same request without creating a duplicate message.'}</span></div>}
        {blocked && <div className="composer__error" role="alert"><span>Message recovery data could not be read. Sending is blocked. Check history to confirm the previous message was not sent.</span><Button size="sm" variant="ghost" onClick={async () => { if (!await confirm({ title: 'Discard recovery data?', description: 'The previous message may already have been received. Check history before sending another message.', confirm: 'Discard message recovery', danger: true })) return; try { sessionStorage.removeItem(storageKey); setBlocked(false); setError(''); nextFrame(() => composer.current?.focus()); } catch { setError('Recovery data could not be deleted. Check site storage.'); } }}>Discard message recovery</Button></div>}
        <QueuedRunsControl turns={queueTurns.filter(queued)} path={path} target={dispatchTarget} request={api} archived={archived} onChanged={refreshTurns} onOrder={() => setQueueOrderOpen(true)} />
        {htmlContext && <div className="html-reply-context"><span role="status">Reply context attached</span><Button type="button" variant="ghost" size="sm" disabled={!!pending || busy} onClick={() => setHtmlContext(undefined)}>Remove context</Button></div>}
        <ChatComposer attachments={attachments.composer} hasAttachments={!!attached.length} onPasteFiles={files => { if (pastedFiles.length) { setError('Wait for pasted files to finish uploading.'); return; } setPastedFiles(files); }} mentions={channel ? members : []} onMention={value => { setDraft(value); try { sessionStorage.setItem(draftKey, value); } catch { setError('Draft was not saved. Copy your message before leaving.'); } }} label={channel ? `Message #${channel.section_name ?? channel.name}` : `Message ${agent.name}`} inputRef={composer} multiline={draft.includes('\n')} onSubmit={send} onSendKey={event => { void send(event, (event.ctrlKey || event.metaKey) && canSteer ? 'steer' : 'queue'); }} onAttach={attachments.choose} attachmentDisabledReason={archived || blocked || !!pending || busy || attachments.uploading ? 'Attachments unavailable' : undefined} actions={attachments.recordingAction}
          recipient={channel && activeTurns.length > 0 ? <Select size="sm" label="Addressed member" value={dispatchTarget?.id ?? ''} disabled={busy || !!pending || blocked} onChange={setAddressedTurn} options={[{ value: '', label: 'Everyone' }, ...activeTurns.map(turn => ({ value: turn.id, label: members.find(member => member.id === turn.agent)?.name ?? turn.agent }))]} /> : undefined}
          stopAction={runningRoom && !archived ? <RoomStop key={runningRoom.root} room={runningRoom} path={path} archived={archived} onStopped={() => { refreshTurns(); composer.current?.focus(); }} /> : runningTurn && !archived ? <CancelTurn key={runningTurn.id} primary turn={runningTurn.id} path={path} request={api} onCancelled={() => { refreshTurns(); composer.current?.focus(); }} /> : undefined}
          input={{ id: 'message', ...(builtin && !turns?.length ? { placeholder: 'Describe your work or ask about Minarbot' } : firstMessage && { placeholder: 'Try: Say hello' }), value: pending?.text ?? draft, readOnly: blocked || !!pending || busy, onChange: event => { setDraft(event.target.value); try { sessionStorage.setItem(draftKey, event.target.value); } catch { setError('Draft was not saved in the browser. Copy the message before switching agents.'); } } }}
          busy={busy} retry={!!pending}
          sendDisabled={!!pastedFiles.length || attachments.uploading || blocked || archived || busy || (!pending && (!canSend || !draft.trim() && !attached.length))}
          sendLabel={busy ? 'Sending...' : pending ? 'Retry send' : dispatchTarget ? 'Queue message' : 'Send message'}
          sendTitle={pending ? 'Retry send' : dispatchTarget ? `Queue message (Enter to queue${canSteer ? ', Ctrl/Cmd+Enter to steer' : ''})` : 'Send (Enter)'} />
        {(focusedTask || before) && <p className="composer__foot">{focusedTask ? channel ? 'Task family shown' : 'Selected message shown' : 'Older messages · 100 per page'}</p>}
      </footer>
      <ChatHeader editing={rename} agent={agent} title={title} onToggle={() => setMenuOpen(!menuOpen)} expanded={menuOpen}
        menu={menuOpen && <Menu align="center" label={`Menu ${title}`} onClose={() => setMenuOpen(false)} items={items} />}
        right={<>
          {!detailsOpen && <button type="button" className="icon-btn icon-btn--bordered" onClick={() => setDetailsOpen(true)} aria-label="Show details" title="Show details (Ctrl/Cmd+Shift+B)" aria-keyshortcuts="Control+Shift+B Meta+Shift+B"><PanelRight size={16} aria-hidden /></button>}</>} />
    </main>
    {detailsOpen && <RightPanel path={path.replace(/\/turns$/, '')} name={agent.name} state={rightPanel} members={channel ? members.map(member => ({ ...member, lead: member.id === channel.lead })) : undefined} request={api} onClose={() => setDetailsOpen(false)} attached={attachments.panelAttached} attachedCount={(pending?.files ?? attached).length} attachDisabled={archived || blocked || !!pending || busy || attachments.uploading} onAttach={file => void attachments.togglePanelFile(file)} settings={onEdit && <IconButton label={editLabel} onClick={onEdit}><Settings size={16} aria-hidden /></IconButton>} details={
      <div className="details__scroll">
        <div className="details__identity">
          <AgentAvatar agent={agent} size={36} />
          <div className="details__identity-text"><div className="details__name" title={agent.name}>{agent.name}</div>
            <div className="details__who-sub" title={!channel ? (agent as Agent).role || 'Agent' : undefined}>{channel ? `Section${channel.archived ? ' · Archived' : ''}` : (agent as Agent).role || 'Agent'}</div>
            {!channel && <span className="details__model" title={(agent as Agent).runtime && (agent as Agent).runtime !== 'api' ? `${(agent as Agent).runtime} runtime` : (agent as Agent).model}>{(agent as Agent).runtime && (agent as Agent).runtime !== 'api' ? `${(agent as Agent).runtime} runtime` : (agent as Agent).model}</span>}
          </div>
        </div>
        {!channel && <div className="details__computer"><AgentComputerPanel key={`computer:${agent.id}`} agent={agent.id} request={api} onOpen={() => rightPanel.select('computer')} /></div>}
        {channel && <div className="details__channel-meta">
          <div className="section-label">{members.length} members</div>
          <div className="details__members" aria-label={`${members.length} channel members`}>{members.map(member => <span key={member.id} role="img" aria-label={`${member.name}${member.id === channel.lead ? ", lead" : ""}`} title={member.name}><AgentAvatar agent={member} size={24} /></span>)}</div>
        </div>}
        <div className="details__sections">
              {detailsExtra?.chats}
            </div>
      </div>
    } activity={<div className="details__sections details-activity">
              {!!waiting.length && <section className="activity-section"><Collapsible variant="section" title={<>Waiting <Badge>{waiting.length}</Badge></>}><div className="k-stack">{waiting.map(({turn, event}) => <div key={event.id} className="k-stack"><button type="button" className="panel-row panel-row--link" onClick={() => openFocused(turn.id)}><span className="panel-row__text"><span className="panel-row__title">{event.event}</span><time className="panel-row__meta" dateTime={new Date(event.deadline).toISOString()}>{new Date(event.deadline).toLocaleString('en-US')}</time></span></button>{!archived && <EventConnection compact path={`/api/turn-waits/${event.id}`} request={api} />}</div>)}</div></Collapsible></section>}
              {channel?.project && <SectionRoutines channel={channel.id} onOpen={onOpenAgent} />}
              {detailsExtra?.onManageRoutines && <UpcomingRoutines agent={agent.id} onManage={detailsExtra.onManageRoutines} />}
            </div>} />}
  </div>;
}

function WorkflowConversation({ agent, threadId, configured, onOpenAgent, onOpenChannel }: { agent?: Agent; threadId: string; configured: boolean; onOpenAgent: (agent: string, thread: string) => void; onOpenChannel: (channel: string) => void }) {
  const [thread, setThread] = useState<Thread | null>(null), [error, setError] = useState(''), [reload, setReload] = useState(0);
  useEffect(() => {
    if (!agent) return;
    let stopped = false; setThread(null); setError('');
    void api<Thread[]>(`/api/agents/${agent.id}/threads`).then(rows => {
      if (stopped) return;
      const current = rows.find(item => item.id === threadId);
      if (current) setThread(current); else setError('Step conversation is no longer available. Return to the run to view saved results.');
    }).catch(error => { if (!stopped) setError(message(error)); });
    return () => { stopped = true; };
  }, [agent?.id, threadId, reload]);
  if (!agent) return <p className="error" role="alert">Step agent is no longer available.</p>;
  if (error) return <p className="error" role="alert">{error} <button className="secondary" onClick={() => setReload(value => value + 1)}>Reload step conversation</button></p>;
  if (!thread) return <p className="empty" role="status">Loading step conversation...</p>;
  return <Chat key={thread.id} agent={agent} thread={thread} configured={(agent.runtime && agent.runtime !== 'api' ? agent.provider_available === true : configured && agent.provider_available !== false)} onOpenAgent={onOpenAgent} onOpenChannel={onOpenChannel} />;
}

type Routine = { webhook_source?: WebhookSource | null; trigger: 'schedule' | 'webhook' | 'manual' | 'script'; id: string; agent: string; name: string; prompt: string; next_at: number; interval_ms: number | null; enabled: number; revision: number; project?: string | null; project_name?: string | null; report_to?: string; report_section?: string | null; schedule?: import('../shared/routine-schedule').Spec | null; schedule_zone?: string | null; routineReport?: { run: string; status: string; thread: string }; owner_name?: string; flow_agents?: number; last_source?: string | null; last_scheduled_at?: number | null; last_status?: string | null; last_paused?: number | null; last_no_update?: number | null; last_created?: string | null; last_thread?: string | null };
type RoutineRun = { paused: number; source: string; no_update: number; turn: string; thread: string; scheduled_at: number | null; created: string; missed: number; status: string; reply: string | null; error: string | null };
type RoutineCommand = { routineId: string | null; data: { id: string; revision?: number; webhookSource?: WebhookSource; trigger?: 'schedule' | 'webhook' | 'manual' | 'script'; name: string; prompt: string; nextAt?: number; intervalSeconds?: number | null; schedule?: import('../shared/routine-schedule').Spec; zone?: string; project?: string | null; reportTo?: string; enabled: boolean } };
const routineReportLabel = (routine: Routine) => routine.report_section ? `${routine.project_name ?? 'Project'} / # ${routine.report_section}` : routine.report_to === 'direct' ? "Owner agent's direct chat" : "Only in the run's own conversation";
function SectionRoutines({channel,onOpen}:{channel:string;onOpen?:(agent:string,thread:string)=>void}) {
  const [rows,setRows]=useState<Routine[] | null>(null),[error,setError]=useState('');
  usePolling(async signal => {try {
    const all:Routine[]=[]; let after='';
    do { const page=await api<{routines:Routine[];next_after:string|null}>(`/api/routines?${new URLSearchParams({reportTo:channel,after})}`);all.push(...page.routines);after=page.next_after ?? ''; } while(after && !signal.aborted);
    if(!signal.aborted){setRows(all);setError('');}
  } catch(error){if(!signal.aborted)setError(message(error));} }, { interval: 5000 }, [channel]);
  if (!error && !rows?.length) return null;
  return <section className="activity-section" aria-label="Routines reporting here"><Collapsible variant="section" title={<>Upcoming <Badge>{rows?.length ?? 0}</Badge></>}>{error && <Alert tone="error">{error}</Alert>}<div className="panel-card">{rows===null ? <p className="panel-empty" role="status">Loading routines...</p> : rows.length ? rows.map(row=><div key={row.id} className="panel-row"><span className="panel-row__text"><span className="panel-row__title">{row.name}</span><span className="panel-row__meta">{routineTriggerLabel(row)} · {row.enabled ? 'Enabled' : 'Paused'}</span>{row.last_thread && <button type="button" className="link-btn link-btn--small" onClick={()=>onOpen?.(row.agent,row.last_thread!)}>Open last run</button>}</span></div>) : <p className="panel-empty">No routines report to this section.</p>}</div></Collapsible></section>;
}
function UpcomingRoutines({ agent, onManage }: { agent: string; onManage: () => void }) {
  const [rows, setRows] = useState<Routine[] | null>(null), [error, setError] = useState('');
  usePolling(async signal => {
    try { const next = await api<Routine[]>(`/api/agents/${agent}/routines`); if (!signal.aborted) { setRows(next); setError(''); } }
    catch (error) { if (!signal.aborted) setError(message(error)); }
  }, { interval: 1200 }, [agent]);
  // ponytail: preview the next three schedules; Manage opens the full routine list.
  const upcoming = rows?.filter(row => row.enabled && row.trigger === 'schedule').sort((a, b) => a.next_at - b.next_at).slice(0, 3);
  if (!error && !upcoming?.length) return null;
  const count = rows?.filter(row => row.enabled && row.trigger === 'schedule').length ?? 0;
  return <section className="activity-section" aria-label="Upcoming routines">
    {error ? <Alert tone="error" actions={<Button size="sm" variant="ghost" onClick={onManage}>Manage</Button>}>{error}</Alert> : <>
      <div className="upcoming-row"><Collapsible variant="section" title={<>Upcoming <Badge>{count}</Badge><time className="panel-row__meta" dateTime={new Date(upcoming![0].next_at).toISOString()}>{new Date(upcoming![0].next_at).toLocaleString('en-US', { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })}</time></>}><div className="panel-card">{upcoming?.map(row => <button type="button" key={row.id} className="panel-row panel-row--routine panel-row--link" onClick={onManage}><Clock size={16} aria-hidden /><span className="panel-row__text"><span className="panel-row__title">{row.name}</span><time className="panel-row__meta" dateTime={new Date(row.next_at).toISOString()}>{new Date(row.next_at).toLocaleString('en-US')}</time></span></button>)}</div></Collapsible><Button size="sm" variant="ghost" className="upcoming-manage" onClick={onManage}>Manage</Button></div>
    </>}
  </section>;
}
function pendingRoutine(agent: string): RoutineCommand | null {
  try {
    const value = JSON.parse(sessionStorage.getItem(`minarbot:routine-command:${agent}`) ?? 'null'), data = value?.data;
    return value && typeof data?.id === 'string' && typeof data.name === 'string' && typeof data.prompt === 'string' && typeof data.enabled === 'boolean'
      && (data.webhookSource === undefined || typeof data.webhookSource === 'string' && data.webhookSource !== 'schedule' && Object.hasOwn(routineTemplates, data.webhookSource) && data.trigger === 'webhook')
      && (data.schedule && typeof data.schedule === 'object' && typeof data.schedule.kind === 'string' && typeof data.zone === 'string'
        || Number.isSafeInteger(data.nextAt) && data.nextAt > 0 && data.nextAt <= 253370764799999 && (data.intervalSeconds === null || Number.isSafeInteger(data.intervalSeconds)))
      && (value.routineId === null || typeof value.routineId === 'string') ? value : null;
  } catch { return null; }
}
function ScriptAccess({ agent }: { agent: string }) {
  const key = `minarbot:script-access:${agent}`;
  const [access, setAccess] = useState<{ enabled: number; revision: number; configured: boolean } | null>(null);
  const [pending, setPending] = useState<{ id: string; revision: number; enabled: boolean } | null>(() => {
    try { const value = JSON.parse(sessionStorage.getItem(key) ?? 'null'); return value && typeof value.id === 'string' && Number.isSafeInteger(value.revision) && typeof value.enabled === 'boolean' ? value : null; } catch { return null; }
  });
  const [error, setError] = useState(''), [busy, setBusy] = useState(false), [reload, setReload] = useState(0);
  const mounted = useRef(true);
  const readVersion = useRef(0);
  useEffect(() => () => { mounted.current = false; }, []);
  useEffect(() => {
    let stopped = false;
    const version = ++readVersion.current;
    void api<NonNullable<typeof access>>(`/api/agents/${agent}/scripts`).then(data => { if (!stopped && version === readVersion.current) { setAccess(data); setError(''); } }).catch(error => { if (!stopped && version === readVersion.current) setError(message(error)); });
    return () => { stopped = true; };
  }, [agent, reload]);
  async function save() {
    if (busy || !access) return;
    const request = pending ?? { id: crypto.randomUUID(), revision: access.revision, enabled: !access.enabled };
    try { sessionStorage.setItem(key, JSON.stringify(request)); } catch { setError('Permission status was not saved in the browser. Check site storage.'); return; }
    readVersion.current++;
    setPending(request); setBusy(true); setError('');
    try {
      await api(`/api/agents/${agent}/scripts`, request);
      const latest = await api<NonNullable<typeof access>>(`/api/agents/${agent}/scripts`);
      if (!mounted.current) return;
      sessionStorage.removeItem(key); setPending(null); setAccess(latest);
    } catch (error) {
      if (!mounted.current) return;
      if (error instanceof HttpError && error.status < 500) { sessionStorage.removeItem(key); setPending(null); setAccess(null); }
      setError(message(error));
    } finally { if (mounted.current) setBusy(false); }
  }
  return <Section title="Local script">
    <p className="k-field__hint">A script with a server token can read and run routines, create conversations, send direct messages, read results, answer questions, approve or reject workflow approvals, and change this agent’s model and provider account. Turning off access does not stop work already accepted.</p>
    {access && <Alert>{access.enabled ? 'Script access is active.' : 'Script access is inactive.'}{!access.configured && ' Script token is not configured on the server.'}</Alert>}
    {pending && <RecoveryAlert inFlight={busy} tone="warn" title="Permission change is not confirmed yet">Save again to check the same request.</RecoveryAlert>}
    {error && <Alert tone="error">{error}</Alert>}
    <div className="k-row-actions"><Button busy={busy} disabled={busy || !access || (!pending && !access.enabled && !access.configured)} onClick={() => void save()}>{busy ? 'Saving permission...' : pending ? 'Retry saving permission' : access?.enabled ? 'Disable script access' : 'Allow script access'}</Button><IconButton label="Reload permission" disabled={busy} onClick={() => setReload(value => value + 1)}><RefreshCw size={15} aria-hidden /></IconButton></div>
  </Section>;
}
type WebhookConfig = { route: string; revision: number; url: string; event_name: string; filters: unknown[]; secret?: string };
function WebhookSettings({ routine, onBack }: { routine: Routine; onBack: () => void }) {
  type Command = { id: string; revision: number; rotate: boolean; eventName: string; filters: unknown[] };
  const key = `minarbot:webhook:${routine.id}`, path = `/api/routines/${routine.id}/webhook`;
  const source = routine.webhook_source;
  const [pending, setPending] = useState<Command | null>(() => {
    if (source) return null;
    try { const value = JSON.parse(sessionStorage.getItem(key) ?? 'null'); return value && typeof value.id === 'string' && Number.isSafeInteger(value.revision) && typeof value.rotate === 'boolean' && typeof value.eventName === 'string' && Array.isArray(value.filters) ? value : null; } catch { return null; }
  });
  const [hook, setHook] = useState<WebhookConfig | null | undefined>(undefined), [secret, setSecret] = useState('');
  const configured = !!hook && hook.revision > 0;
  const [events, setEvents] = useState<{ delivery: string; received: number; result: { outcome: string; reason?: string } }[]>([]);
  const [error, setError] = useState(''), [busy, setBusy] = useState(false), [conflict, setConflict] = useState(false), [reload, setReload] = useState(0);
  const mounted = useRef(true), loadVersion = useRef(0);
  useEffect(() => () => { mounted.current = false; }, []);
  useEffect(() => {
    let stopped = false; const version = ++loadVersion.current;
    void Promise.all([api<WebhookConfig | null>(path), api<typeof events>(`/api/routines/${routine.id}/webhook-events`)]).then(([config, receipts]) => {
      if (!stopped && version === loadVersion.current) { setHook(config); setEvents(receipts); setConflict(false); setError(''); }
    }).catch(error => { if (!stopped && version === loadVersion.current) setError(message(error)); });
    return () => { stopped = true; };
  }, [path, routine.id, reload]);
  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault(); if (busy || conflict || hook === undefined) return;
    const fields = new FormData(event.currentTarget);
    const signingSecret = fields.get('signingSecret');
    let request: Command | { id: string; revision: number; filters: unknown[]; signingSecret?: string };
    try {
      const base = { id: crypto.randomUUID(), revision: hook?.revision ?? 0, filters: pending?.filters ?? JSON.parse(String(fields.get('filters'))) };
      if (!Array.isArray(base.filters)) throw new Error('Filter must be a JSON list.');
      if (source) {
        request = { ...base, ...(typeof signingSecret === 'string' && signingSecret ? { signingSecret } : {}) };
      } else {
        const command = pending ?? { ...base, rotate: !hook || fields.get('rotate') === 'on', eventName: String(fields.get('eventName')) };
        sessionStorage.setItem(key, JSON.stringify(command));
        setPending(command);
        request = command;
      }
    } catch { setError('Filter must be a valid JSON list and browser storage must be available.'); return; }
    loadVersion.current++; setBusy(true); setError(''); setSecret('');
    try {
      const result = await api<WebhookConfig>(path, request);
      const latest = await api<WebhookConfig | null>(path);
      if (!mounted.current) return;
      if (!latest || latest.revision !== result.revision || latest.route !== result.route) throw new HttpError(409, 'Webhook changed. Reload before using the secret.');
      if (!source) sessionStorage.removeItem(key);
      setPending(null); setHook(latest); setSecret(source ? '' : result.secret ?? '');
    } catch (error) {
      if (!mounted.current) return;
      if (source) setConflict(error instanceof HttpError && error.status === 409);
      else if (error instanceof HttpError && error.status < 500) { sessionStorage.removeItem(key); setPending(null); setConflict(true); }
      setError(message(error));
    } finally { if (mounted.current) setBusy(false); }
  }
  return <div className="k-page bs-root">
    <PageHeader title={source ? `${routineTemplates[source].name} webhook` : `Webhook ${routine.name}`} description={source ? undefined : 'The local URL receives signed events while Minarbot is running. Payloads are sent to the provider. This URL is not available from the internet.'}
      actions={<><Button variant="ghost" onClick={onBack}>Back</Button><IconButton label="Reload webhook" disabled={busy} onClick={() => { setSecret(''); setReload(value => value + 1); }}><RefreshCw size={15} aria-hidden /></IconButton></>} />
    {hook && <Field label="Webhook URL"><Input readOnly value={hook.url} onFocus={event => event.target.select()} /></Field>}
    {secret && <Field label="New secret" hint="Copy this to the sender’s secret storage. The secret is not saved in the browser and will not be shown again after the page closes."><Input readOnly value={secret} autoComplete="off" spellCheck={false} onFocus={event => event.target.select()} /></Field>}
    {hook === undefined ? <p role="status">Loading webhook...</p> : <form className="k-fieldset" key={`${hook?.route ?? 'new'}:${hook?.revision ?? 0}:${reload}`} onSubmit={save}>
      {source ? <Field label="Signing secret"><Input name="signingSecret" type="password" autoComplete="new-password" spellCheck={false} required={!configured} minLength={16} maxLength={512} placeholder={configured ? 'Enter new secret to rotate' : undefined} /></Field> :
        <Field label="Event name" hint="Leave empty for all events."><Input name="eventName" maxLength={256} readOnly={!!pending} defaultValue={pending?.eventName ?? hook?.event_name ?? ''} /></Field>}
      <Field label="Payload filters" hint={source ? undefined : <>JSON filter list, example: {'[{"pointer":"/environment","equals":"production"}]'}. All filters must match.</>}><Textarea name="filters" rows={4} readOnly={!!pending} defaultValue={JSON.stringify(pending?.filters ?? hook?.filters ?? [], null, 2)} /></Field>
      {hook && !source && <label className="k-check"><input type="checkbox" name="rotate" disabled={!!pending} defaultChecked={pending?.rotate ?? false} />Rotate secret. The old secret becomes invalid immediately.</label>}
      {pending && <RecoveryAlert inFlight={busy} tone="warn" title="Save is not confirmed yet">Try again with the same request.</RecoveryAlert>}
      <div><Button type="submit" variant="primary" busy={busy} disabled={busy || conflict}>{busy ? 'Saving...' : pending ? 'Retry saving webhook' : configured ? 'Save webhook' : source ? 'Enable webhook' : 'Create URL and secret'}</Button></div>
    </form>}
    {error && <Alert tone="error">{error}</Alert>}
    <Section title="Received events">{!events.length ? <EmptyState title="No saved events">Reload to see new events.</EmptyState> : <Card>{events.map(event => <Row key={`${event.delivery}:${event.received}`} title={event.delivery} meta={<><span>{new Date(event.received).toLocaleString('en-US')}</span><Badge>{event.result.outcome === 'queued' ? 'Accepted for execution' : 'Ignored'}</Badge></>}>{event.result.reason && <span className="k-field__hint">{event.result.reason}</span>}</Row>)}</Card>}</Section>
  </div>;
}
type FlowRun = { root_turn: string; created: string; steps: { agent: string; name: string; status: string; task_status: string | null; paused: number | null; thread: string | null; result: string; result_length: number }[] };
function FlowDiagram({ graph, agents }: { graph: FlowGraph; agents: Agent[] }) {
  const marker = useId();
  const depth = new Map<string, number>(), counts = new Map<number, number>(), positions = new Map<string, { x: number; y: number }>();
  for (const node of graph.nodes) {
    const level = Math.max(0, ...graph.links.filter(link => link.to === node).map(link => (depth.get(link.from) ?? 0) + 1));
    depth.set(node, level); positions.set(node, { x: 12 + level * 200, y: 12 + (counts.get(level) ?? 0) * 84 }); counts.set(level, (counts.get(level) ?? 0) + 1);
  }
  const name = (id: string) => agents.find(agent => agent.id === id)?.name ?? id;
  return <div className="bs-root" style={{ overflowX: 'auto', border: '0.5px solid var(--color-border-tertiary)', borderRadius: 14, padding: 8, background: 'var(--color-bg-chrome)' }} tabIndex={0} role="region" aria-label="Flow diagram, scroll to view the full graph"><svg width={Math.max(360, (Math.max(...depth.values()) + 1) * 200)} height={Math.max(...counts.values()) * 84} role="img" aria-label="Agent relationships in flow"><defs><marker id={marker} viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse"><path d="M 0 0 L 10 5 L 0 10 z" fill="var(--color-brand)" /></marker></defs>{graph.links.map(link => { const start = positions.get(link.from)!, end = positions.get(link.to)!; return <path key={`${link.from}:${link.to}`} d={`M ${start.x + 160} ${start.y + 26} L ${end.x - 4} ${end.y + 26}`} fill="none" stroke="var(--color-brand)" strokeWidth="2" markerEnd={`url(#${marker})`} />; })}{graph.nodes.map(node => { const pos = positions.get(node)!; return <g key={node} transform={`translate(${pos.x},${pos.y})`}><title>{name(node)}</title><rect width="160" height="52" rx="7" fill="var(--color-bg-pane)" stroke="var(--color-border-primary)" /><text x="12" y="31" fill="var(--color-text-primary)" fontSize="13">{name(node).length > 20 ? `${name(node).slice(0, 18)}...` : name(node)}</text></g>; })}</svg></div>;
}
function FlowEditor({ routine, onOpen }: { routine: Routine; onOpen: (thread: string, owner: string) => Promise<void> }) {
  const optionName = (name: string) => name.length > 24 ? name.slice(0, 21) + '...' : name;
  type Command = { id: string; revision: number; links: FlowLink[] };
  const key = `minarbot:flow:${routine.id}`, path = `/api/routines/${routine.id}/flow`;
  const [pending, setPending] = useState<Command | null>(() => {
    try { const value = JSON.parse(sessionStorage.getItem(key) ?? 'null'); return value && typeof value.id === 'string' && Number.isSafeInteger(value.revision) && Array.isArray(value.links) ? value : null; } catch { return null; }
  });
  const [agents, setAgents] = useState<Agent[]>([]), [links, setLinks] = useState<FlowLink[]>([]), [revision, setRevision] = useState(0);
  const [runs, setRuns] = useState<FlowRun[]>([]), [runError, setRunError] = useState('');
  const [from, setFrom] = useState(routine.agent), [to, setTo] = useState('');
  const [loaded, setLoaded] = useState(false), [error, setError] = useState(''), [busy, setBusy] = useState(false), [conflict, setConflict] = useState(false), [reload, setReload] = useState(0);
  const mounted = useRef(true);
  useEffect(() => () => { mounted.current = false; }, []);
  useEffect(() => {
    let stopped = false; setLoaded(false);
    void Promise.all([api<{ graph: FlowGraph; revision: number }>(path), api<Directory>('/api/agents')]).then(([flow, directory]) => {
      if (!stopped) { setAgents(directory.agents); setLinks(pending?.links ?? flow.graph.links); setRevision(flow.revision); setLoaded(true); setError(''); setConflict(false); }
    }).catch(error => { if (!stopped) setError(message(error)); });
    return () => { stopped = true; };
  }, [path, reload]);
  usePolling(async signal => {
    try { const rows = await api<FlowRun[]>(`/api/routines/${routine.id}/flow-runs`); if (!signal.aborted) { setRuns(rows); setRunError(''); } } catch (error) { if (!signal.aborted) setRunError(message(error)); }
  }, { interval: 1200 }, [routine.id]);
  const locked = !loaded || busy || !!pending || conflict;
  let graph: FlowGraph = { root: routine.agent, nodes: [routine.agent], links: [] };
  try { if (loaded) graph = validateFlow(links, routine.agent, new Set(agents.map(agent => agent.id))); } catch { /* Server validates recovered requests; keep the editable edge list visible. */ }
  function change(next: FlowLink[]) {
    if (locked) return;
    try { validateFlow(next, routine.agent, new Set(agents.map(agent => agent.id))); setLinks(next); setError(''); } catch (error) { setError(message(error)); }
  }
  async function save() {
    if (!loaded || busy || conflict) return;
    const request = pending ?? { id: crypto.randomUUID(), revision, links };
    try { sessionStorage.setItem(key, JSON.stringify(request)); } catch { setError('Flow was not saved in the browser. Check site storage.'); return; }
    setPending(request); setBusy(true); setError('');
    try {
      await api(path, request);
      const latest = await api<{ graph: FlowGraph; revision: number }>(path);
      if (!mounted.current) return;
      sessionStorage.removeItem(key); setPending(null); setLinks(latest.graph.links); setRevision(latest.revision);
    } catch (error) {
      if (!mounted.current) return;
      if (error instanceof HttpError && error.status < 500) { sessionStorage.removeItem(key); setPending(null); setConflict(true); }
      setError(message(error));
    } finally { if (mounted.current) setBusy(false); }
  }
  const name = (id: string) => agents.find(agent => agent.id === id)?.name ?? id;
  return <div className="k-stack bs-root routine-flow">
    <Section title="Agent flow"
      actions={<><IconButton label="Reload flow" disabled={busy} onClick={() => setReload(value => value + 1)}><RefreshCw size={15} aria-hidden /></IconButton><Button variant="primary" busy={busy} disabled={!loaded || busy || conflict} onClick={() => void save()}>{busy ? 'Saving...' : pending ? 'Retry saving flow' : 'Save flow'}</Button></>}><p className="k-field__hint">Connect routine results to the next agent. Multiple inputs wait for all results; branches without a successful result are skipped. Changes apply only to new runs.</p></Section>
    {!loaded ? <p role="status">Loading flow...</p> : <><FlowDiagram graph={graph} agents={agents} />
      <div className="k-grid-2"><Field label="From agent" hint={name(from)}><Select value={from} disabled={locked} onChange={value => { setFrom(value); if (to === value) setTo(''); }} options={graph.nodes.map(id => ({ value: id, label: optionName(name(id)) }))} /></Field><Field label="To agent" hint={to ? name(to) : 'Not selected'}><Select value={to} disabled={locked} onChange={setTo} options={[{ value: '', label: 'Select agent' }, ...agents.filter(agent => agent.id !== routine.agent && agent.id !== from).map(agent => ({ value: agent.id, label: optionName(agent.name) }))]} /></Field></div>
      <div><Button disabled={locked || !to || to === from} onClick={() => change([...links, { from, to }])}>Connect</Button></div>
      <Card>{links.map(link => <Row key={`${link.from}:${link.to}`} title={`${name(link.from)} to ${name(link.to)}`} trailing={<IconButton label={`Remove relationship ${name(link.from)} to ${name(link.to)}`} disabled={locked} onClick={() => { const remaining = links.filter(item => item !== link), reachable = new Set([routine.agent]); let changed = true; while (changed) { changed = false; for (const edge of remaining) if (reachable.has(edge.from) && !reachable.has(edge.to)) { reachable.add(edge.to); changed = true; } } change(remaining.filter(edge => reachable.has(edge.from))); setFrom(routine.agent); }}><X size={14} aria-hidden /></IconButton>} />)}</Card>
      <p className="k-field__hint">Removing a relationship also removes branches no longer connected to the origin.</p>
    </>}
    {pending && <RecoveryAlert inFlight={busy} tone="warn" title="Save is not confirmed yet">Try again with the same request.</RecoveryAlert>}{error && <Alert tone="error">{error}</Alert>}
    <Section title="Execution steps">{runError && <Alert tone="error">{runError}</Alert>}{!runs.length ? <EmptyState title="No runs with saved flows" /> : runs.map(run => <Section key={run.root_turn} title={new Date(run.created).toLocaleString('en-US')}><Card>{run.steps.map(step => <Row key={step.agent} title={step.name} meta={<Badge>{step.paused ? 'Paused' : step.status === 'waiting' ? 'Waiting for input' : step.status === 'skipped' ? 'Skipped' : statusNames[step.task_status ?? step.status] ?? step.status}</Badge>} trailing={step.thread && <Button size="sm" onClick={() => void onOpen(step.thread!, step.agent).catch(error => setRunError(message(error)))}>Open step {step.name}</Button>}>
      {step.result && <span className="k-field__hint" style={{ whiteSpace: 'pre-wrap' }}>{step.result}{step.result_length > 1000 ? '...' : ''}</span>}
    </Row>)}</Card></Section>)}</Section>
  </div>;
}

function routineTriggerLabel(routine: Routine) {
  if (routine.trigger === 'webhook') return routine.webhook_source ? routineTemplates[routine.webhook_source].name : 'Webhook';
  if (routine.trigger !== 'schedule') return routine.trigger === 'script' ? 'Local script' : 'Manual';
  return scheduleLabel(routine.schedule, routine.next_at, routine.interval_ms, routine.schedule_zone);
}
function relativeRun(value: string | number) {
  const time = typeof value === 'number' ? value : Date.parse(value), seconds = Math.round((time - Date.now()) / 1000), absolute = Math.abs(seconds);
  const unit = absolute < 60 ? 'second' : absolute < 3600 ? 'minute' : absolute < 86400 ? 'hour' : 'day';
  return new Intl.RelativeTimeFormat('en', { numeric: 'auto' }).format(Math.round(seconds / (unit === 'day' ? 86400 : unit === 'hour' ? 3600 : unit === 'minute' ? 60 : 1)), unit);
}
const runStatus = (run: { paused?: number | null; no_update?: number | null; status?: string | null }) => run.paused ? 'Paused' : run.no_update ? 'No updates' : statusNames[run.status ?? ''] ?? run.status ?? 'Never run';
function RoutinesHub({ agents, projects, initialAgent, onOpen }: { agents: Agent[]; projects: import('../shared/local-project').LocalProject[]; initialAgent?: string; onOpen: (owner: string, thread: string) => void }) {
  type Command = { routine: string; action: 'test' | 'edit' | 'delete'; data: { id: string; revision: number } & Record<string, unknown> };
  const key = 'minarbot:routine-hub-command';
  const [recovered] = useState(() => {
    try {
      const raw = sessionStorage.getItem(key); if (!raw) return { command: null, blocked: false };
      const value = JSON.parse(raw);
      if (!value || !['test','edit','delete'].includes(value.action) || !/^[a-f0-9-]{36}$/i.test(value.routine) || !/^[a-f0-9-]{36}$/i.test(value.data?.id) || !Number.isSafeInteger(value.data?.revision)) throw new Error('Invalid command');
      return { command: value as Command, blocked: false };
    } catch { return { command: null, blocked: true }; }
  });
  const [pending, setPending] = useState<Command | null>(recovered.command), [blocked, setBlocked] = useState(recovered.blocked), [busy, setBusy] = useState(false);
  const [error, setError] = useState(''), [notice, setNotice] = useState(''), [reload, setReload] = useState(0);
  const [filters, setFilters] = useState({ search: '', agent: initialAgent ?? '', enabled: '', trigger: '', project: '' }), [cursors, setCursors] = useState(['']);
  const [editing, setEditing] = useState<Routine | null | undefined>(undefined), [detail, setDetail] = useState<Routine | null>(null), [webhook, setWebhook] = useState<Routine | null>(null);
  const [pickingTemplate, setPickingTemplate] = useState(false), [template, setTemplate] = useState<RoutineTemplate>();
  const heading = useRef<HTMLHeadingElement>(null), confirm = useConfirm(), flight = useRef(false), mounted = useRef(true), loadVersion = useRef(0);
  const after = cursors.at(-1)!;
  const path = `/api/routines?${new URLSearchParams({ ...filters, after })}`;
  const { data: page, fresh } = useCachedData<{ routines: Routine[]; next_after: string | null }>(path);
  useEffect(() => () => { mounted.current = false; }, []);
  useQueryPolling(async signal => {
    const version = loadVersion.current, cacheVersion = queryCache.version();
    try {
      const data = await queryCache.refresh(path, () => api<NonNullable<typeof page>>(path));
      if (!signal.aborted && cacheVersion === queryCache.version() && version === loadVersion.current) setDetail(current => current ? data.routines.find(row => row.id === current.id) ?? current : null);
    } catch (error) { if (!signal.aborted) setError(message(error)); }
  }, { interval: 1200 }, [filters, after, reload]);
  useEffect(() => { if (!detail) heading.current?.focus(); }, [!!detail, after]);
  const locked = busy || !!pending || blocked;
  function filter(name: keyof typeof filters, value: string) { setCursors(['']); setFilters(current => ({ ...current, [name]: value })); }
  const owner = (id: string) => agents.find(agent => agent.id === id);
  async function command(request: Command) {
    if (flight.current || blocked) return;
    flight.current = true; loadVersion.current++; setBusy(true); setError(''); setNotice('');
    try {
      // Retain the original request before sending; retries never regenerate an ID or payload.
      sessionStorage.setItem(key, JSON.stringify(request)); setPending(request);
      const result = await api<Routine>(`/api/routines/${request.routine}/${request.action}`, request.data);
      sessionStorage.removeItem(key);
      if (request.action === 'test') sessionStorage.removeItem(`minarbot:routine-test:${request.routine}`);
      if (!mounted.current) return;
      loadVersion.current++; setPending(null);
      if (request.action === 'edit') setDetail(current => current?.id === request.routine ? { ...current, ...result } : current);
      setReload(value => value + 1);
      setNotice(request.action === 'test' ? 'Run accepted. Open Overview to follow its progress.' : request.action === 'delete' ? 'Routine deleted. Existing run conversations are retained.' : 'Routine updated.');
      if (request.action === 'delete') setDetail(null);
    } catch (error) {
      if (!mounted.current) return;
      if (error instanceof HttpError && error.status < 500) { sessionStorage.removeItem(key); if (request.action === 'test') sessionStorage.removeItem(`minarbot:routine-test:${request.routine}`); setPending(null); setReload(value => value + 1); }
      setError(message(error));
    } finally { flight.current = false; if (mounted.current) setBusy(false); }
  }
  function toggle(routine: Routine) {
    if (locked || !fresh) return;
    void command({ routine: routine.id, action: 'edit', data: { id: crypto.randomUUID(), revision: routine.revision, name: routine.name, prompt: routine.prompt, ...(routine.schedule ? { schedule: routine.schedule, zone: routine.schedule_zone } : { nextAt: routine.next_at, intervalSeconds: routine.interval_ms === null ? null : routine.interval_ms / 1000 }), project: routine.project, reportTo: routine.report_to, enabled: !routine.enabled, trigger: routine.trigger } });
  }
  function run(routine: Routine) {
    if (locked) return;
    try {
      const saved = sessionStorage.getItem(`minarbot:routine-test:${routine.id}`), data = saved ? JSON.parse(saved) : { id: crypto.randomUUID(), revision: routine.revision };
      if (!data || typeof data.id !== 'string' || !Number.isSafeInteger(data.revision)) throw new Error('Previous run request could not be read. Check browser storage and run history before retrying.');
      void command({ routine: routine.id, action: 'test', data });
    } catch (error) { setError(message(error)); }
  }
  async function remove(routine: Routine) {
    if (locked || !await confirm({ title: `Delete ${routine.name}?`, description: 'Future triggers stop. Existing conversations and run records are retained. Complete or cancel an active run before deleting.', confirm: 'Delete routine', danger: true })) return;
    void command({ routine: routine.id, action: 'delete', data: { id: crypto.randomUUID(), revision: routine.revision } });
  }
  async function discard() {
    if (!await confirm({ title: 'Discard routine recovery?', description: 'Check routines and run history first. The previous request may already have succeeded.', confirm: 'Discard recovery', danger: true })) return;
    try { sessionStorage.removeItem(key); setPending(null); setBlocked(false); setReload(value => value + 1); } catch (error) { setError(message(error)); }
  }
  const recovery = <>{blocked && <Alert tone="error" title="Routine recovery could not be read" actions={<Button disabled={busy} onClick={() => void discard()}>Discard recovery</Button>}>Check routine settings and run history before discarding.</Alert>}{pending && <RecoveryAlert inFlight={busy} tone="warn" title="Request is not confirmed yet" actions={<><Button disabled={busy} busy={busy} onClick={() => void command(pending)}>Retry request</Button><Button disabled={busy} onClick={() => void discard()}>Discard recovery</Button></>}>Retry the original request to check its result without duplicating work.</RecoveryAlert>}{error && <Alert tone="error" actions={<Button size="sm" onClick={() => { setError(''); setReload(value => value + 1); }}>Reload routines</Button>}>{error}</Alert>}{notice && <p role="status" className="k-field__hint">{notice}</p>}</>;
  const hasFilters = Object.values(filters).some(Boolean) || !!after;
  const showFilters = !!page?.routines.length || hasFilters;
  const recovering = agents.some(agent => pendingRoutine(agent.id));
  const newRoutine = agents.length > 0 && <Button disabled={locked} variant="primary" icon={<Plus size={14} aria-hidden />} onClick={() => { setTemplate(undefined); setEditing(null); }}>{recovering ? 'Resume saving' : 'New routine'}</Button>;
  const editingOwner = editing ? owner(editing.agent) : agents.find(agent => pendingRoutine(agent.id)) ?? owner(filters.agent) ?? agents[0];
  return <>
    {webhook ? <WebhookSettings key={webhook.id} routine={webhook} onBack={() => setWebhook(null)} /> : detail ? <RoutineDetail routine={detail} locked={locked || !fresh} recovery={recovery} onBack={() => { setDetail(null); setReload(value => value + 1); }} onEdit={() => setEditing(detail)} onRun={() => run(detail)} onToggle={() => toggle(detail)} onDelete={() => void remove(detail)} onWebhook={() => setWebhook(detail)} onOpen={onOpen} /> : <div className="k-page bs-root routines-hub">
      <PageHeader headingRef={heading} title="Routines" actions={agents.length > 0 && <><Button disabled={locked || recovering} onClick={() => setPickingTemplate(true)}>New from template</Button>{showFilters && newRoutine}</>} />
      {showFilters && <div className="routine-filters"><Field label="Search routines"><Input type="search" maxLength={80} value={filters.search} onChange={event => filter('search', event.target.value)} placeholder="Search by name" /></Field><Field label="Agent"><Select value={filters.agent} onChange={value => filter('agent', value)} options={[{ value: '', label: 'All agents' }, ...agents.map(agent => ({ value: agent.id, label: agent.name }))]} /></Field><Field label="Status"><Select value={filters.enabled} onChange={value => filter('enabled', value)} options={[{ value: '', label: 'All statuses' }, { value: '1', label: 'Enabled' }, { value: '0', label: 'Disabled' }]} /></Field><Field label="Project"><Select value={filters.project} onChange={value => filter('project', value)} options={[{ value: '', label: 'All projects' }, { value: 'none', label: 'No project' }, ...projects.map(project => ({ value: project.id, label: project.name }))]} /></Field><Field label="Trigger"><Select value={filters.trigger} onChange={value => filter('trigger', value)} options={[{ value: '', label: 'All triggers' }, { value: 'once', label: 'Once' }, { value: 'recurring', label: 'Recurring schedule' }, { value: 'webhook', label: 'Webhook' }, { value: 'manual', label: 'Manual' }, { value: 'script', label: 'Local script' }]} /></Field></div>}
      {recovery}
      {!page ? <><span className="sr-only" role="status">Loading routines...</span><SkeletonRows /></> : !page.routines.length ? <EmptyState icon={<CalendarClock size={20} aria-hidden />} title={hasFilters ? 'No matching routines' : 'No routines yet'} action={hasFilters ? <Button onClick={() => { setFilters({ search: '', agent: '', enabled: '', trigger: '', project: '' }); setCursors(['']); }}>Clear filters</Button> : newRoutine}></EmptyState> : <div className="k-table-wrap"><table className="k-table routine-table"><thead><tr><th>Routine</th><th>Owner</th><th>Project</th><th>Report to</th><th>Trigger</th><th>Last run</th><th>Next run</th><th>Enabled</th></tr></thead><tbody>{page.routines.map(row => <tr key={row.id}><td><button type="button" className="routine-name k-link" disabled={!fresh} onClick={() => { setNotice(''); setDetail(row); }}>{row.name}</button>{Number(row.flow_agents) > 1 && <Badge>{row.flow_agents} agents</Badge>}</td><td><span className="routine-owner">{owner(row.agent) && <AgentAvatar agent={owner(row.agent)!} size={24} />}<span>{row.owner_name}</span></span></td><td>{row.project_name ?? <span className="k-field__hint">No project</span>}</td><td>{routineReportLabel(row)}</td><td>{routineTriggerLabel(row)}</td><td>{row.last_status && row.last_thread ? <button type="button" className="routine-run k-link" onClick={() => onOpen(row.agent, row.last_thread!)}><span>{runStatus({ status: row.last_status, paused: row.last_paused, no_update: row.last_no_update })}</span><time dateTime={row.last_created!}>{relativeRun(row.last_created!)}</time></button> : <span className="k-field__hint">Never run</span>}</td><td>{row.enabled && row.trigger === 'schedule' ? <time title={new Date(row.next_at).toLocaleString('en-US')} dateTime={new Date(row.next_at).toISOString()}>{relativeRun(row.next_at)}</time> : <span className="k-field__hint">{row.enabled ? 'On demand' : 'Paused'}</span>}</td><td><Switch checked={!!row.enabled} disabled={locked || !fresh} label={`Enable ${row.name}`} onChange={() => toggle(row)} /></td></tr>)}</tbody></table></div>}
      {(cursors.length > 1 || !!page?.next_after) && <div className="k-pager"><IconButton label="Previous routines" disabled={cursors.length < 2} onClick={() => setCursors(values => values.slice(0, -1))}><ChevronLeft size={16} aria-hidden /></IconButton><IconButton label="Next routines" disabled={!page?.next_after} onClick={() => { if (page?.next_after) setCursors(values => [...values, page.next_after!]); }}><ChevronRight size={16} aria-hidden /></IconButton></div>}
    </div>}
    {pickingTemplate && <RoutineTemplatePicker onClose={() => setPickingTemplate(false)} onPick={value => { setTemplate(value); setPickingTemplate(false); setEditing(null); }} />}
    {editing !== undefined && editingOwner && <RoutineForm key={editing?.id ?? 'new'} owner={editingOwner} agents={agents} projects={projects} routine={editing} template={editing ? undefined : template} onSaved={updated => {
      if (!editing && updated.webhook_source) { setDetail(updated); setWebhook(updated); }
      else setDetail(current => current?.id === updated.id ? { ...current, ...updated } : current);
      setEditing(undefined); setReload(value => value + 1);
    }} onBack={() => { setEditing(undefined); setReload(value => value + 1); }} />}
  </>;
}
function RoutineDetail({ routine, locked, recovery, onBack, onEdit, onRun, onToggle, onDelete, onWebhook, onOpen }: { routine: Routine; locked: boolean; recovery: ReactNode; onBack: () => void; onEdit: () => void; onRun: () => void; onToggle: () => void; onDelete: () => void; onWebhook: () => void; onOpen: (owner: string, thread: string) => void }) {
  const [tab, setTab] = useState('overview'), [runs, setRuns] = useState<RoutineRun[] | null>(null), [error, setError] = useState('');
  const heading = useRef<HTMLHeadingElement>(null);
  useEffect(() => { heading.current?.focus(); }, []);
  usePolling(async signal => {
    try { const data = await api<RoutineRun[]>(`/api/routines/${routine.id}/runs`); if (!signal.aborted) { setRuns(data); setError(''); } } catch (error) { if (!signal.aborted) setError(message(error)); }
  }, { interval: 1200 }, [routine.id]);
  return <div className="k-page bs-root routine-detail"><PageHeader headingRef={heading} title={routine.name} description={`Owned by ${routine.owner_name ?? 'local agent'}`} actions={<><Button variant="ghost" onClick={onBack}>All routines</Button><Button disabled={locked} icon={<Pencil size={14} aria-hidden />} onClick={onEdit}>Edit routine</Button><Button disabled={locked} icon={routine.enabled ? <Pause size={14} aria-hidden /> : <Play size={14} aria-hidden />} onClick={onToggle}>{routine.enabled ? 'Pause' : 'Resume'}</Button><Button disabled={locked} variant="primary" icon={<Play size={14} aria-hidden />} onClick={onRun}>Run now</Button><ActionMenu label="Routine actions" trigger={<button type="button" className="k-icon-btn" aria-label="Routine actions" disabled={locked}><Ellipsis size={16} aria-hidden /></button>} items={[{ label: 'Delete routine', danger: true, onSelect: onDelete }]} /></>} />
    {recovery}
    <Tabs value={tab} onChange={setTab} label="Routine sections" tabs={[{ value: 'overview', label: 'Overview' }, { value: 'flow', label: 'Flow' }]}>
      <TabPanel value="overview"><div className="k-stack"><Card><Row title="Trigger" meta={routineTriggerLabel(routine)} trailing={<Badge>{routine.enabled ? 'Enabled' : 'Paused'}</Badge>} /><Row title="Project" meta={routine.project_name ?? 'No project'} /><Row title="Report to" meta={routineReportLabel(routine)} /><Row title="Next run" meta={routine.enabled && routine.trigger === 'schedule' ? new Date(routine.next_at).toLocaleString('en-US') : routine.enabled ? 'On demand' : 'Paused'} /><Row title="Last run" meta={routine.last_created ? <>{runStatus({ status: routine.last_status, paused: routine.last_paused, no_update: routine.last_no_update })} · {relativeRun(routine.last_created)}</> : 'Never run'} trailing={routine.last_thread && <Button size="sm" onClick={() => onOpen(routine.agent, routine.last_thread!)}>Open last run</Button>} /><Row title="Flow" meta={Number(routine.flow_agents ?? 1) > 1 ? `${routine.flow_agents} agents` : 'Single agent'} /></Card>
      <Section title="Instructions"><p className="routine-instructions">{routine.prompt}</p></Section>
      <div className="k-row-actions">{routine.trigger === 'webhook' && <Button onClick={onWebhook}>Webhook settings</Button>}<a className="k-link" href={`/api/agents/${routine.agent}/routines/calendar.ics`} download="minarbot-routines.ics">Download agent calendar</a></div>
      <Section title="Run history">{error && <Alert tone="error">{error}</Alert>}{runs === null ? <SkeletonRows /> : !runs.length ? <EmptyState title="No runs yet" /> : <Card>{runs.map(run => <Row key={run.turn} title={new Date(run.scheduled_at ?? run.created).toLocaleString('en-US')} meta={<><Badge>{runStatus(run)}</Badge><span>{run.source === 'webhook' ? 'Webhook' : run.source === 'script' ? 'Local script' : run.scheduled_at === null ? 'Manual' : 'Schedule'} · {relativeRun(run.created)}</span></>} trailing={<Button size="sm" onClick={() => onOpen(routine.agent, run.thread)}>Open run</Button>}>{run.missed > 0 && <p className="k-field__hint">{run.missed} missed schedules were combined into this run.</p>}{run.error && <Alert tone="error">{run.error}</Alert>}</Row>)}</Card>}</Section>
      <Collapsible title="Local script access"><ScriptAccess key={routine.agent} agent={routine.agent} /></Collapsible>
      </div></TabPanel>
      <TabPanel value="flow"><FlowEditor key={routine.id} routine={routine} onOpen={async (thread, owner) => onOpen(owner, thread)} /></TabPanel>
    </Tabs>
  </div>;
}
function RoutineForm({ owner, agents, projects, routine, template, onBack, onSaved }: { owner: Agent; agents: Agent[]; projects: import('../shared/local-project').LocalProject[]; routine: Routine | null; template?: RoutineTemplate; onBack: () => void; onSaved: (routine: Routine) => void }) {
  const [agent, setAgent] = useState(owner);
  const key = `minarbot:routine-command:${agent.id}`;
  const pending = useRef<RoutineCommand | null>(pendingRoutine(owner.id));
  const [initial] = useState(() => pending.current);
  const [blocked, setBlocked] = useState(() => { try { return sessionStorage.getItem(key) !== null && !pending.current; } catch { return true; } });
  const confirm = useConfirm();
  const routineId = initial?.routineId ?? routine?.id ?? null;
  const revision = initial?.data.revision ?? routine?.revision;
  const [conflict, setConflict] = useState(false);
  const snapshot = initial?.data;
  const preset = template ? routineTemplates[template] : undefined;
  const webhookSource = snapshot?.webhookSource ?? routine?.webhook_source ?? (template && template !== 'schedule' ? template : undefined);
  const savedTrigger = snapshot?.trigger ?? routine?.trigger ?? (webhookSource ? 'webhook' : 'schedule');
  const [trigger, setTrigger] = useState<Routine['trigger']>(savedTrigger);
  const [enabled, setEnabled] = useState(snapshot?.enabled ?? (routine ? !!routine.enabled : true));
  const [project,setProject] = useState(snapshot?.project ?? routine?.project ?? null), [reportTo,setReportTo] = useState(snapshot?.reportTo ?? routine?.report_to ?? 'direct');
  const [timing,setTiming] = useState<RoutineTiming | null>(null);
  const [error, setError] = useState(''), [busy, setBusy] = useState(false);
  const mounted = useRef(true);
  useEffect(() => () => { mounted.current = false; }, []);
  const [scheduleStart] = useState(snapshot?.nextAt ?? routine?.next_at ?? Date.now() + 3600000);
  const scheduleInitial = snapshot?.schedule ?? routine?.schedule ?? (routine ? routine.interval_ms ? { kind:'legacy_interval' as const,intervalMs:routine.interval_ms,anchorAt:routine.next_at } : { kind:'instant' as const,at:routine.next_at } : null);
  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault(); if (busy || conflict || blocked || !pending.current && trigger === 'schedule' && !timing) return;
    const fields = new FormData(event.currentTarget);
    const request: RoutineCommand = pending.current ?? { routineId, data: { id: crypto.randomUUID(), ...(routineId ? { revision } : {}), name: String(fields.get('name')).trim(), prompt: String(fields.get('prompt')).trim(), trigger,
      ...(!routineId && webhookSource && trigger === 'webhook' ? { webhookSource } : {}),
      ...(trigger === 'schedule' && timing ? timing : { nextAt: scheduleStart, intervalSeconds: routine?.interval_ms === null || !routine ? null : routine.interval_ms / 1000 }), project, reportTo, enabled } };
    try { sessionStorage.setItem(key, JSON.stringify(request)); } catch { setError('Save status was not saved in the browser. Check site storage.'); return; }
    pending.current = request; setBusy(true); setError('');
    try {
      const updated = await api<Routine>(request.routineId ? `/api/routines/${request.routineId}/edit` : `/api/agents/${agent.id}/routines`, request.data);
      if (!mounted.current) return;
      if (pendingRoutine(agent.id)?.data.id === request.data.id) sessionStorage.removeItem(key);
      pending.current = null; onSaved(updated);
    } catch (error) {
      if (!mounted.current) return;
      if (error instanceof HttpError && error.status < 500) { if (pendingRoutine(agent.id)?.data.id === request.data.id) sessionStorage.removeItem(key); pending.current = null; }
      if (error instanceof HttpError && error.status === 409) setConflict(true);
      setError(message(error));
    } finally { if (mounted.current) setBusy(false); }
  }
  return <Dialog open size="lg" onOpenChange={open => { if (!open && !busy) onBack(); }} title={initial?.routineId || routine ? 'Edit routine' : 'New routine'}
    description={preset ? undefined : 'Each run opens a separate conversation and can share its result. Minarbot must be running for schedules.'}
    footer={<><span className="k-dialog__foot-start"><Checkbox checked={enabled} onChange={setEnabled} disabled={!!pending.current || busy || blocked} label="Enabled" /></span><Button disabled={busy} onClick={onBack}>{conflict ? 'Discard changes and reload' : 'Cancel'}</Button><Button type="submit" form="routine-form" variant="primary" busy={busy} disabled={busy || conflict || blocked || !pending.current && trigger === 'schedule' && !timing}>{busy ? 'Saving...' : pending.current ? 'Retry save' : 'Save routine'}</Button></>}>
    <form id="routine-form" className="k-fieldset bs-root" onSubmit={save}>
      {blocked && <Alert tone="error" title="Routine recovery could not be read" actions={<Button onClick={async () => {
        if (!await confirm({ title: 'Discard routine save recovery?', description: 'Check routines and run history first. The previous save may already have succeeded.', confirm: 'Discard recovery', danger: true })) return;
        try { sessionStorage.removeItem(key); setBlocked(false); } catch { setError('Browser storage is unavailable. Check site storage before saving.'); }
      }}>Discard recovery</Button>}>Check saved routines before discarding this request.</Alert>}
      <div className="k-grid-2"><Field label="Owner agent" hint={routineId ? 'The owner cannot change after creation.' : undefined}><Select value={agent.id} disabled={!!routineId || !!pending.current || busy || blocked} onChange={id => { const next = agents.find(item => item.id === id); if (next) {setAgent(next); if(project && !projects.find(item=>item.id===project)?.members.some(member=>member.agent===next.id)) {setProject(null);setReportTo('direct');}} }} options={agents.map(item => ({ value: item.id, label: item.name, disabled: item.id !== agent.id && !!pendingRoutine(item.id) }))} /></Field>
      <Field label="Name"><Input name="name" required maxLength={80} readOnly={!!pending.current} defaultValue={snapshot?.name ?? routine?.name ?? preset?.name} placeholder="Morning briefing" /></Field></div>
      <Field label="Instructions"><Textarea name="prompt" required maxLength={16000} readOnly={!!pending.current} defaultValue={snapshot?.prompt ?? routine?.prompt ?? preset?.prompt} rows={3} placeholder="What should this routine do?" /></Field>
      <Field label="Trigger"><Select value={trigger} disabled={!!pending.current || busy} onChange={value=>setTrigger(value as Routine['trigger'])} options={[{ value: 'schedule', label: 'Schedule' }, { value: 'manual', label: 'Manual' }, { value: 'webhook', label: webhookSource ? routineTemplates[webhookSource].name : 'Webhook' }, { value: 'script', label: 'Local script' }]} /></Field>
      {trigger === 'webhook' && !webhookSource && <Alert>After saving, open Webhook settings to create a URL and secret. Changing this trigger removes the old URL and secret.</Alert>}
      {trigger === 'script' && <Alert>After saving, allow Local script access in the routine overview. Scripts use the existing server token and this routine's ID.</Alert>}
      <RoutineSchedule initial={scheduleInitial} initialNextAt={scheduleStart} initialInterval={snapshot?.intervalSeconds === undefined ? routine?.interval_ms ?? null : snapshot.intervalSeconds === null ? null : snapshot.intervalSeconds*1000} initialZone={snapshot?.zone ?? routine?.schedule_zone} legacy={!snapshot?.schedule && !routine?.schedule} disabled={!!pending.current || busy || blocked} active={trigger === 'schedule'} onChange={setTiming} />
      <div className="k-grid-2"><Field label="Project" hint="Project runs use its folders and access grants."><Select value={project ?? ''} disabled={!!pending.current || busy || blocked} onChange={value=>{setProject(value || null);setReportTo(value ? 'run' : 'direct');}} options={[{value:'',label:'No project'},...projects.map(item=>({value:item.id,label:item.name,disabled:!item.members.some(member=>member.agent===agent.id),hint:item.members.some(member=>member.agent===agent.id) ? undefined : 'Owner agent must be a member'}))]} /></Field><Field label="Report to" hint="Reports never start replies from other agents."><Select value={reportTo} disabled={!!pending.current || busy || blocked} onChange={setReportTo} options={[{value:'direct',label:"Owner agent's direct chat"},{value:'run',label:"Only in the run's own conversation"},...(projects.find(item=>item.id===project)?.sections.filter(section=>!section.archived).map(section=>({value:section.id,label:`${projects.find(item=>item.id===project)!.name} / # ${section.name}`})) ?? [])]} /></Field></div>
      {routine && <p className="k-field__hint">Disabling or changing a schedule cancels queued runs; runs already in progress continue.</p>}
      {pending.current && <RecoveryAlert inFlight={busy} tone="warn" title="Save is not confirmed yet">Retry the same request to check its result without creating a duplicate routine.</RecoveryAlert>}
      {error && <Alert tone="error">{error}</Alert>}
    </form>
  </Dialog>;
}

function AgentConversations({ agent, attention, onRestored, configured, firstRun, onEdit, onExport, workflowAgents, initialThread, onOpenAgent, onOpenChannel, onSelectThread, onManageRoutines }: { onRestored: () => Promise<void>; onExport: () => void; onManageRoutines: (agent: string) => void; onOpenChannel: (channel: string) => void; agent: Agent; attention: ReadonlySet<string> | undefined; workflowAgents: Agent[]; configured: boolean; firstRun?: boolean; onEdit: () => void; initialThread: string; onSelectThread: (thread: string, replace?: boolean) => void; onOpenAgent: (owner: string, thread: string) => void }) {
  const [threads, setThreads] = useState<Thread[]>([]), [error, setError] = useState('');
  const selected = initialThread;
  const [form, setForm] = useState<Thread | null>(null), [loaded, setLoaded] = useState(false);
  const [showArchived, setShowArchived] = useState(false), [busy, setBusy] = useState(false);
  const confirm = useConfirm();
  const commandKey = `minarbot:thread-command:${agent.id}`;
  const [recovery] = useState(() => { try { return { command: restoreThreadCommand(sessionStorage.getItem(commandKey), agent.id), invalid: false }; } catch { return { command: null, invalid: true }; } });
  const [pending, setPending] = useState<ThreadCommand | null>(recovery.command), [invalid, setInvalid] = useState(recovery.invalid);
  const flight = useRef(false), actionFlight = useRef(false), renameTarget = useRef('');
  const [showWorkspace, setShowWorkspace] = useState(false);
  const workspaceButton = useRef<HTMLButtonElement>(null);
  const nextFrame = useNextFrame();
  const [showWorkflows, setShowWorkflows] = useState(false);
  const { navigation, loadVersion } = useConversationRequests();
  const thread = threads.find(item => item.id === selected);
  function select(id: string, replace = false) { setShowWorkspace(false); setShowWorkflows(false); navigation.current++; loadVersion.current++; onSelectThread(id, replace); setForm(null); }
  async function load(preferred = selected, afterSave = false, quiet = false, signal?: AbortSignal) {
    const version = ++loadVersion.current;
    try {
      const rows = await api<Thread[]>(`/api/agents/${agent.id}/threads`);
      if (signal?.aborted || version !== loadVersion.current) return;
      const missing = !!preferred && !rows.some(row => row.id === preferred);
      setThreads(rows); setLoaded(true);
      if (!quiet) setError('');
      if (afterSave) select(missing ? '' : preferred, missing);
    }
    catch (error) { if (!afterSave && !quiet && version === loadVersion.current) setError(message(error)); throw new Error(message(error)); }
  }
  usePolling(signal => load(selected, false, loaded, signal).catch(() => {}), { interval: 5000, enabled: !loaded || !busy && !pending && !invalid }, [selected]);
  const leave = () => { navigation.current++; loadVersion.current++; };
  const openForm = () => { if (!busy && !pending && !invalid) void execute({ path: `/api/agents/${agent.id}/threads`, data: { id: crypto.randomUUID(), name: 'New conversation' } }); };
  useNewConversationShortcut(openForm);
  const openRoutines = () => { leave(); onManageRoutines(agent.id); };
  const openWorkflows = () => { leave(); setShowWorkspace(false); setShowWorkflows(true); };
  const openWorkspace = () => { leave(); setShowWorkflows(false); setForm(null); setShowWorkspace(true); };
  const errorBanner = error && <p className="error" role="alert">{error} <button className="secondary" onClick={() => void load().catch(() => {})}>Reload conversation</button></p>;
  if (showWorkspace) return <PageView>{errorBanner}<Suspense fallback={<Delayed><p className="empty" role="status">Loading workspace files...</p></Delayed>}><WorkspacePanel key={agent.id} agent={agent} request={api} onClose={() => { setShowWorkspace(false); nextFrame(() => workspaceButton.current?.focus()); }} /></Suspense></PageView>;
  if (showWorkflows) return <PageView wide>{errorBanner}<Suspense fallback={<Delayed><p className="empty" role="status">Loading workflows...</p></Delayed>}><WorkflowPanel owner={agent} agents={workflowAgents} request={api} onNavigate={leave} onBack={() => { leave(); setShowWorkflows(false); }} renderConversation={(id, owner) => <WorkflowConversation key={id} agent={workflowAgents.find(item => item.id === owner)} threadId={id} configured={configured} onOpenAgent={onOpenAgent} onOpenChannel={onOpenChannel} />} /></Suspense></PageView>;

  if (!loaded) return <PageView>{errorBanner}<p className="empty" role="status">Loading conversation...</p></PageView>;
  if (selected && !thread) return <PageView><EmptyState title="Page not found" action={<Button onClick={() => select('')}>Back to chats</Button>}>This conversation no longer exists.</EmptyState></PageView>;
  const locked = busy || !!pending || invalid;
  async function execute(command: ThreadCommand) {
    if (flight.current || invalid) return;
    flight.current = true; setBusy(true); setError('');
    const version = navigation.current;
    try {
      sessionStorage.setItem(commandKey, JSON.stringify(command)); setPending(command);
      const result = await api<Thread>(command.path, command.data);
      sessionStorage.removeItem(commandKey); setPending(null); setForm(null);
      if (version === navigation.current) {
        const hidden = result.deleted || result.archived && !showArchived;
        await load(command.path === `/api/agents/${agent.id}/threads` ? result.id : selected === result.id && hidden ? '' : selected, true);
      }
    } catch (error) {
      if (error instanceof HttpError && error.status < 500) { sessionStorage.removeItem(commandKey); setPending(null); }
      setError(message(error));
    } finally { flight.current = false; setBusy(false); }
  }
  async function change(item: Thread, action: 'archive' | 'restore' | 'delete') {
    if (locked || actionFlight.current) return;
    actionFlight.current = true; setBusy(true);
    const version = navigation.current;
    try {
      if (action === 'delete') {
        const rows = await api<Thread[]>(`/api/agents/${agent.id}/threads`);
        if (version !== navigation.current) return;
        const latest = rows.find(row => row.id === item.id);
        if (!latest) { await load('', true); return; }
        item = latest;
        if (item.active) { await confirm({ title: 'Stop work first', description: 'This conversation has running or queued work. Open it and stop unfinished turns before deleting it.', confirm: 'Got it' }); return; }
      }
      if (action !== 'restore' && !await confirm({ title: `${action === 'delete' ? 'Delete' : 'Archive'} conversation?`, description: action === 'delete' ? `“${item.name}” and its history will no longer be available. This cannot be undone.` : `“${item.name}” will be hidden. History stays saved; use Show archived to restore it. Unfinished work will be stopped.`, confirm: action === 'delete' ? 'Delete conversation' : 'Archive conversation', danger: true })) return;
      if (version !== navigation.current) return;
      await execute({ path: `/api/threads/${item.id}/${action === 'delete' ? 'delete' : 'edit'}`, data: { id: crypto.randomUUID(), revision: item.revision, ...(action === 'delete' ? {} : { name: item.name, archived: action === 'archive' }) } });
    } catch (error) { if (version === navigation.current) setError(message(error)); }
    finally { actionFlight.current = false; setBusy(false); }
  }
  const actions = (item: Thread): MenuItem[] => [
    { label: 'Rename', icon: <Pencil size={15} aria-hidden />, disabled: locked, onSelect: () => { renameTarget.current = item.id; setForm(item); } },
    { label: item.archived ? 'Restore' : 'Archive', icon: <Archive size={15} aria-hidden />, disabled: locked, onSelect: () => void change(item, item.archived ? 'restore' : 'archive') },
    { label: 'Delete', icon: <Trash2 size={15} aria-hidden />, separator: true, danger: true, disabled: locked, onSelect: () => void change(item, 'delete') },
  ];
  const nameEditor = form && <InlineConversationName key={`${form.id}:${form.revision}`} thread={form} busy={locked} onCancel={() => { renameTarget.current = ''; setForm(null); nextFrame(() => document.querySelector<HTMLButtonElement>(`[data-conversation-id="${form.id}"] > .panel-row`)?.focus()); }} onSave={name => void execute({ path: `/api/threads/${form.id}/edit`, data: { id: crypto.randomUUID(), name, revision: form.revision, archived: !!form.archived } })} />;
  const row = (item: Thread) => <div data-conversation-id={item.id} key={item.id} className={`conversation-row${selected === item.id ? ' is-current' : ''}`}>
    {form?.id === item.id && selected !== item.id ? nameEditor : <button type="button" className="panel-row panel-row--link" aria-current={selected === item.id || undefined} title={item.name} onClick={() => select(item.id)}><MessageSquare size={16} aria-hidden /><span className="panel-row__text"><span className="panel-row__title">{item.name}</span>{!!item.archived && <span className="panel-row__meta">Archived</span>}</span><NeedsYouBadge count={attention ? Number(attention.has(item.id)) : item.needs_you} /></button>}
    <ActionMenu onCloseAutoFocus={event => { if (renameTarget.current === item.id && document.querySelector('.conversation-rename input')) { event.preventDefault(); renameTarget.current = ''; nextFrame(() => { const input = document.querySelector<HTMLInputElement>('.conversation-rename input'); input?.focus(); input?.select(); }); } }} label={`Menu conversation ${item.name}`} trigger={<IconButton label={`Menu conversation ${item.name}`} tooltip="More actions" disabled={locked}><Ellipsis size={15} aria-hidden /></IconButton>} items={actions(item)} />
  </div>;
  const visible = threads.filter(item => showArchived || !item.archived), deliveries = visible.filter(item => item.delivery);
  const conversations = <>
    {error && <Alert tone="error" actions={<Button size="sm" disabled={busy} onClick={() => void load().catch(() => {})}>Reload conversations</Button>}>{error}</Alert>}
    {(pending || invalid) && <RecoveryAlert inFlight={busy} tone="warn" title={invalid ? 'Conversation recovery is unavailable' : 'Conversation change is not confirmed'} actions={<>{pending && <Button size="sm" busy={busy} onClick={() => void execute(pending)}>Retry change</Button>}<Button size="sm" disabled={busy} onClick={async () => { if (await confirm({ title: 'Discard pending change?', description: 'Check conversations first. The change may already have been saved.', confirm: 'Discard', danger: true })) { try { sessionStorage.removeItem(commandKey); setPending(null); setInvalid(false); } catch { setError('Recovery could not be cleared. Check browser storage.'); } } }}>Discard</Button></>} />}
    <section aria-label="Agent conversations">
      <div className="section-label section-label--row"><span>Conversations</span><div className="details-list-actions"><DropdownMenu.Root><DropdownMenu.Trigger asChild><IconButton label="Conversation filters"><Filter size={15} aria-hidden /></IconButton></DropdownMenu.Trigger><DropdownMenu.Portal><DropdownMenu.Content className="k-pop k-menu" align="end" sideOffset={6}><DropdownMenu.CheckboxItem className="k-menu__item" checked={showArchived} onCheckedChange={next => { setShowArchived(next); if (!next && thread?.archived) select(''); }}><DropdownMenu.ItemIndicator><Check size={14} aria-hidden /></DropdownMenu.ItemIndicator>Show archived</DropdownMenu.CheckboxItem></DropdownMenu.Content></DropdownMenu.Portal></DropdownMenu.Root>{!locked && <IconButton label="New conversation" tooltip="New conversation (Ctrl/Cmd+Shift+O)" aria-keyshortcuts="Control+Shift+O Meta+Shift+O" onClick={openForm}><Plus size={15} aria-hidden /></IconButton>}</div></div>
      <div className="panel-card">
        <div className="conversation-row" data-conversation-id="main"><button type="button" className={`panel-row panel-row--link${!thread ? ' is-current' : ''}`} aria-current={!thread || undefined} onClick={() => select('')}><MessageSquare size={16} aria-hidden /><span className="panel-row__title">Main chat</span><NeedsYouBadge count={Number(attention?.has('') ?? false)} /></button><ActionMenu label="Menu conversation Main chat" trigger={<IconButton label="Menu conversation Main chat" tooltip="More actions"><Ellipsis size={15} aria-hidden /></IconButton>} items={[{ label: 'Edit profile', onSelect: onEdit }]} /></div>
        {visible.filter(item => !item.delivery).map(row)}
      </div>
      {!!deliveries.length && <Collapsible variant="section" title={<>From other agents <Badge>{deliveries.length}</Badge></>}><div className="panel-card">{deliveries.map(row)}</div></Collapsible>}
    </section>
  </>;
  return <><Chat onRestored={onRestored} onOpenChannel={onOpenChannel} onOpenAgent={onOpenAgent} key={thread?.id ?? agent.id} agent={agent} thread={thread} firstRun={firstRun} configured={(agent.runtime && agent.runtime !== 'api' ? agent.provider_available === true : configured && agent.provider_available !== false)}
    onEdit={thread ? () => setForm(thread) : onEdit}
    menuItems={[{ label: 'New conversation', icon: <Plus size={15} aria-hidden />, onSelect: openForm, disabled: locked }, ...(thread ? actions(thread).map((item, index) => ({ ...item, separator: index === 0 || item.separator })) : []), { label: 'Routines', separator: !!thread, icon: <CalendarClock size={15} aria-hidden />, onSelect: openRoutines }, { label: 'Workflows', icon: <WorkflowIcon size={15} aria-hidden />, prefetch: WorkflowPanel.prefetch, onSelect: openWorkflows }, { label: 'Workspace files', prefetch: WorkspacePanel.prefetch, icon: <FolderOpen size={15} aria-hidden />, onSelect: openWorkspace }, { label: 'Export template', prefetch: AgentTemplateExport.prefetch, icon: <Download size={15} aria-hidden />, onSelect: onExport }]}
    detailsExtra={{ chats: conversations, onManageRoutines: openRoutines }} rename={form?.id === selected ? nameEditor : null} /></>;
}

function InlineConversationName({ thread, busy, onSave, onCancel }: { thread: Thread; busy: boolean; onSave: (name: string) => void; onCancel: () => void }) {
  const [name, setName] = useState(thread.name), [error, setError] = useState('');
  return <form className="conversation-rename" noValidate onSubmit={event => { event.preventDefault(); if (busy) return; if (!name.trim()) { setError('Enter a conversation name.'); return; } onSave(name.trim()); }}>
    <Input aria-label="Conversation name" aria-invalid={!!error} maxLength={80} value={name} disabled={busy} autoFocus onFocus={event => event.currentTarget.select()} onChange={event => { setName(event.target.value); setError(''); }} onKeyDown={event => { if (event.key === 'Escape' && !busy) { event.preventDefault(); onCancel(); } }} />
    <IconButton type="submit" label="Save conversation" disabled={busy}><Check size={14} aria-hidden /></IconButton><IconButton label="Cancel rename" disabled={busy} onClick={onCancel}><X size={14} aria-hidden /></IconButton>
    {error && <span className="conversation-rename__error" role="alert">{error}</span>}
  </form>;
}


function ChannelForm({ agents, channel, onCreated, onCancel, onReload }: { agents: Agent[]; channel?: Channel; onCreated: (channel: Channel) => void; onCancel: () => void; onReload?: () => void }) {
  const confirm = useConfirm();
  const storageKey = `minarbot:channel-command:${channel?.id ?? 'new'}`;
  const [recovery] = useState(() => {
    try { return { command: restoreChannelCommand(sessionStorage.getItem(storageKey), !!channel), blocked: false }; }
    catch { return { command: null, blocked: true }; }
  });
  const [pending, setPending] = useState<ChannelCommand | null>(recovery.command), [blocked, setBlocked] = useState(recovery.blocked);
  const [name, setName] = useState(pending?.name ?? channel?.name ?? ''), [instructions, setInstructions] = useState(pending?.instructions ?? channel?.instructions ?? '');
  const [members, setMembers] = useState<string[]>(pending?.members ?? channel?.members ?? []), [lead, setLead] = useState(pending ? pending.lead ?? '' : channel?.lead ?? '');
  const [archived, setArchived] = useState(pending?.archived ?? !!channel?.archived);
  const [error, setError] = useState(''), [busy, setBusy] = useState(false);
  const mounted = useRef(true), sending = useRef(false), nameInput = useRef<HTMLInputElement>(null);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const locked = busy || !!pending || blocked;
  async function discard() {
    if (!await confirm({ title: 'Discard section recovery data?', description: 'Changes may already be saved. Check the list and section settings before creating a new request.', confirm: 'Discard', danger: true })) return;
    try { sessionStorage.removeItem(storageKey); setPending(null); setBlocked(false); setError(''); nameInput.current?.focus(); }
    catch { setError('Recovery data could not be deleted. Check site storage.'); }
  }
  async function create(event: FormEvent<HTMLFormElement>) {
    event.preventDefault(); if (sending.current || blocked) return;
    const data = pending ?? { id: crypto.randomUUID(), ...(channel ? { revision: channel.revision, archived } : {}), name: name.trim(), instructions: instructions.trim(), members, lead: lead || null };
    try { sessionStorage.setItem(storageKey, JSON.stringify(data)); }
    catch { setError('Section request was not saved in the browser. Check site storage before trying again.'); return; }
    sending.current = true; setPending(data); setBusy(true); setError('');
    try {
      const saved = await api<Channel>(channel ? `/api/channels/${channel.id}/edit` : '/api/channels', data);
      if (!mounted.current) return;
      sessionStorage.removeItem(storageKey); setPending(null); onCreated(saved);
    }
    catch (error) {
      if (!mounted.current) return;
      if (error instanceof HttpError && error.status < 500) {
        try { sessionStorage.removeItem(storageKey); setPending(null); }
        catch { setBlocked(true); }
      }
      setError(message(error));
    }
    finally { sending.current = false; if (mounted.current) setBusy(false); }
  }
  return <Dialog open onOpenChange={next => { if (!next && !busy) onCancel(); }} title={channel ? 'Section settings' : 'New section'}
    footer={<>
      {channel && <Button variant="ghost" className="k-dialog__foot-start" disabled={locked} onClick={async () => {
        if (!await confirm({ title: archived ? 'Restore section?' : 'Archive section?', description: archived ? 'Save changes to restore this section.' : 'Archive section and cancel unfinished work when you save changes.', confirm: archived ? 'Restore' : 'Archive', danger: !archived })) return;
        setArchived(!archived);
      }}>{archived ? 'Restore section' : 'Archive section'}</Button>}
      {(pending || blocked) && <Button variant="ghost" disabled={busy} onClick={() => void discard()}>Discard section recovery</Button>}
      {channel && error && !pending && !blocked && onReload && <Button variant="ghost" onClick={onReload}>Discard changes and reload</Button>}
      <Button onClick={onCancel}>Cancel</Button><Button type="submit" form="channel-form" variant="primary" busy={busy} disabled={busy || blocked || !members.length}>{busy ? 'Saving...' : pending ? 'Retry saving section' : channel ? 'Save section' : 'Create section'}</Button>
    </>}>
    {blocked && <Alert tone="error" title="Section recovery data could not be read">Check the list and section settings before discarding recovery.</Alert>}
    {pending && <RecoveryAlert inFlight={busy} tone="warn" title="Request result is not confirmed yet">Try the same request to check the result without creating a duplicate section.</RecoveryAlert>}
    <form id="channel-form" className="k-fieldset bs-root" onSubmit={create}>
      <Field label="Section name"><Input ref={nameInput} name="name" required maxLength={80} readOnly={locked} value={name} onChange={event => setName(event.target.value)} autoFocus /></Field>
      <Field label="Purpose"><Textarea name="instructions" required maxLength={8000} rows={3} readOnly={locked} value={instructions} onChange={event => setInstructions(event.target.value)} /></Field>
      <fieldset disabled={locked} className="k-fieldset"><legend className="k-field__label">Members</legend>{agents.map(agent => <Checkbox key={agent.id} label={agent.name} disabled={locked} checked={members.includes(agent.id)} onChange={checked => { setMembers(checked ? [...members, agent.id] : members.filter(id => id !== agent.id)); if (!checked && lead === agent.id) setLead(''); }} />)}</fieldset>
      <Field label="Lead" hint="Optional. Gives a short reply when everyone stays quiet."><Select value={lead} disabled={locked} onChange={setLead} options={[{ value: '', label: 'No lead' }, ...agents.filter(agent => members.includes(agent.id)).map(agent => ({ value: agent.id, label: agent.name }))]} /></Field>
      {channel && archived !== !!channel.archived && <Alert>{archived ? 'Section will be archived and unfinished work cancelled when you save changes.' : 'Section will be restored when you save changes.'}</Alert>}
      {error && <Alert tone="error">{error}</Alert>}
    </form>
  </Dialog>;
}


const settingsGroups: { label: string; items: { id: SettingsSection; label: string; icon: typeof Settings }[] }[] = [
  { label: 'Preferences', items: [{ id: 'general', icon: SlidersHorizontal, label: 'General' }, { id: 'appearance', icon: Palette, label: 'Appearance' }] },
  { label: 'Agents', items: [{ id: 'skills', icon: BookOpen, label: 'Skills' }] },
  { label: 'Connections', items: [{ id: 'providers', icon: Server, label: 'Providers' }, { id: 'engines', icon: Cpu, label: 'Engines' }, { id: 'hub', icon: LinkIcon, label: 'minarbot.com' }, { id: 'connected-apps', icon: Plug, label: 'Connected apps' }, { id: 'email', icon: Mail, label: 'Email' }, { id: 'mcp', icon: Cable, label: 'MCP connections' }, { id: 'vault', icon: KeyRound, label: 'Vault' }] },
  { label: 'System', items: [{ id: 'machines', icon: Monitor, label: 'Machines' }, { id: 'devices', icon: Smartphone, label: 'Devices' }, { id: 'computer', icon: Monitor, label: 'Computer' }, { id: 'updates', icon: Download, label: 'Updates' }] },
  { label: 'Data', items: [{ id: 'usage', icon: BarChart3, label: 'Usage' }, { id: 'data', icon: Database, label: 'Shared data' }, { id: 'backup', icon: Archive, label: 'Backup & restore' }] },
  { label: 'Advanced', items: [{ id: 'mcp-server', icon: KeyRound, label: 'External access' }] },
];
type Dialog = 'agent' | 'project' | 'edit' | null;
const collapsedKey = 'minarbot:sidebar-collapsed';
const machineGroupsKey = 'minarbot:sidebar-machine-groups';
const sidebarTabKey = 'minarbot:sidebar-tab';
type SidebarTab = 'agents' | 'projects';
const machineStatus = { online: 'Online', stale: 'Stale', offline: 'Offline', unauthorized: 'Unauthorized', 'version-mismatch': 'Version mismatch' };

function MachineGroup({ name, subtitle, count, status, expanded, onExpandedChange, local, children }: {
  name: string; subtitle?: string; count: number; status?: keyof typeof machineStatus; expanded: boolean;
  onExpandedChange: (expanded: boolean) => void; local?: boolean; children: ReactNode;
}) {
  return <Disclosure.Root className="machine-group" open={expanded} onOpenChange={onExpandedChange}>
    <Disclosure.Trigger className="machine-group__header" title={subtitle}>
      <ChevronRight className="machine-group__chevron" size={14} aria-hidden />{local ? <Monitor size={15} aria-hidden /> : <Server size={15} aria-hidden />}
      <span className="machine-group__name">{name}</span><span className="machine-group__count" aria-label={`${count} agents`}>{count}</span>
      {status && <span className={`machine-dot is-${status}`} role="img" aria-label={machineStatus[status]} title={machineStatus[status]} />}
    </Disclosure.Trigger>
    <Disclosure.Content className="machine-group__content" role="group" aria-label={`${name} contents`}>{children}</Disclosure.Content>
  </Disclosure.Root>;
}


function App() {
  const { route, navigate } = useAppRoute();
  const switcher = useQuickSwitcher(), switcherFrame = useNextFrame(), focusComposer = useComposerFocus();
  const { deleteAgent, deleting } = useDeleteAgent(api, async id => {
    setDialog(null); setAgentContent(null);
    if (route.agent === id) navigate({}, true);
    await load(true); await projectState.reload();
  });
  const [directory, setDirectory] = useState<Directory | null>(null);
  const [guideConnectionOpen, setGuideConnectionOpen] = useState(false);
  const guide = directory?.guide;
  const sidebarAgents = directory ? [...directory.agents, ...(guide ? [guide] : [])] : [];
  const defaultAgent = directory?.agents.find(agent => !agent.hidden && (agent.runnable ?? !!directory.configured));
  const selected = route.agent ?? defaultAgent?.id ?? '';
  const [channels, setChannels] = useState<Channel[]>([]);
  const selectedChannel = route.room ?? '';
  const [environment, setEnvironment] = useState<Environment | null>(null), [summaries, setSummaries] = useState<SidebarSummary[]>([]);
  const [routineAgent, setRoutineAgent] = useState('');
  const view = route.view ?? (route.machine ? 'remote' : 'chat');
  const setView = (view: NonNullable<AppRoute['view']>) => navigate({ view });
  const remoteMachinesState = useMachines(api);
  const remoteSelection = route.machine && route.remoteAgent ? { machine: route.machine, agent: route.remoteAgent } : null;
  const [workspaceDialog, setWorkspaceDialog] = useState<'import' | 'help' | null>(null);
  const importAfterNewMenu = useRef(false);
  const [agentContent, setAgentContent] = useState<{ agent: Agent; kind: 'export' } | null>(null);
  const settings = route.settings ?? null;
  const updates = useUpdateEverything(api, remoteMachinesState, environment?.name ?? 'This device', settings === 'updates');
  const setSettings = (next: SettingsSection | null | ((current: SettingsSection | null) => SettingsSection | null)) => navigate(current => ({ ...current, settings: (typeof next === 'function' ? next(current.settings ?? null) : next) ?? undefined }));
  const [settingsQuery, setSettingsQuery] = useState('');
  const settingsSearch = useRef<HTMLInputElement>(null);
  const availableSettings = settingsGroups.map(group => ({ ...group, items: group.items.filter(item => item.id !== 'hub' || directory?.hubAvailable) }));
  const filteredSettings = availableSettings.map(group => ({ ...group, items: group.items.filter(item => settingsQuery.trim().toLocaleLowerCase().split(/\s+/).every(term => `${group.label} ${item.label}`.toLocaleLowerCase().includes(term))) })).filter(group => group.items.length);
  useEffect(() => {
    if (!settings) { setSettingsQuery(''); return; }
    const searchShortcut = (event: globalThis.KeyboardEvent) => {
      if (event.defaultPrevented || event.isComposing || event.key !== '/' || event.ctrlKey || event.metaKey || event.altKey) return;
      if (event.target instanceof HTMLElement && event.target.closest('input, textarea, select, [contenteditable]:not([contenteditable="false"])')) return;
      event.preventDefault(); settingsSearch.current?.focus();
    };
    window.addEventListener('keydown', searchShortcut);
    return () => window.removeEventListener('keydown', searchShortcut);
  }, [settings]);
  const projectState = useProjects(api);
  const selectedProject = route.project ?? '';
  const routeThread = useRouteThread<Thread & { project?: string; folder?: string }>(route.thread, route.agent, api);
  const projectConversation = routeThread?.thread?.project ? routeThread.thread : null;
  const projectThread = projectConversation ? { project: projectConversation.project!, agent: selected, thread: projectConversation.id, conversation: projectConversation } : null;
  const [dialog, setDialog] = useState<Dialog>(null);
  const [firstRunAgent, setFirstRunAgent] = useState('');
  const [notificationsMuted, setNotificationsMuted] = useState(() => { try { return localStorage.getItem(notificationMuteKey) === '1'; } catch { return false; } });
  const [notificationError, setNotificationError] = useState('');
  const [sidebarTab, setSidebarTab] = useState<SidebarTab>(() => { try { return localStorage.getItem(sidebarTabKey) === 'projects' ? 'projects' : 'agents'; } catch { return 'agents'; } });
  const [searches, setSearches] = useState({ agents: '', projects: '' });
  const search = searches[sidebarTab], query = search.trim().toLocaleLowerCase();
  const [searchMachineGroups, setSearchMachineGroups] = useState<Record<string, boolean>>({});
  const [onlyOnline, setOnlyOnline] = useState(false), [onlyNeedsYou, setOnlyNeedsYou] = useState(false), [filterMenu, setFilterMenu] = useState(false);
  const [machineGroups, setMachineGroups] = useState<Record<string, boolean>>(() => { try { const value: unknown = JSON.parse(localStorage.getItem(machineGroupsKey) ?? '{}'); return value && typeof value === 'object' && !Array.isArray(value) ? Object.fromEntries(Object.entries(value).filter(([, expanded]) => typeof expanded === 'boolean')) as Record<string, boolean> : {}; } catch { return {}; } });
  const [desktopCollapsed, setCollapsedState] = useState(() => { try { return localStorage.getItem(collapsedKey) === '1'; } catch { return false; } });
  const narrowSidebar = useNarrowSidebar(), [narrowOpen, setNarrowOpen] = useState(false);
  const collapsed = narrowSidebar ? !narrowOpen : desktopCollapsed;
  const [detailsVisible] = useDetailsOpen();
  const sidebar = useResizablePanel(sidebarPanel, detailsVisible ? 280 : 0);
  const [agentActionError, setAgentActionError] = useState('');
  const [activityError, setActivityError] = useState(false);
  const [editReload, setEditReload] = useState(0);
  const [error, setError] = useState('');
  const directoryRequest = useRef(0);
  function setCollapsed(value: boolean) { if (narrowSidebar) { setNarrowOpen(!value); return; } setCollapsedState(value); try { localStorage.setItem(collapsedKey, value ? '1' : '0'); } catch { /* Applies to this page only. */ } }
  function setMachineExpanded(key: string, expanded: boolean) { if (query) { setSearchMachineGroups(current => ({ ...current, [key]: expanded })); return; } setMachineGroups(current => { const next = { ...current, [key]: expanded }; try { localStorage.setItem(machineGroupsKey, JSON.stringify(next)); } catch { /* Applies to this page only. */ } return next; }); }
  useEffect(() => { setSearchMachineGroups({}); }, [search, sidebarTab]);
  useEffect(() => { try { localStorage.setItem(sidebarTabKey, sidebarTab); } catch { /* Keep tabs usable without storage. */ } }, [sidebarTab]);
  async function load(propagate = false) {
    const version = ++directoryRequest.current;
    try { const [data, rooms, local, summary] = await Promise.all([api<Directory>('/api/agents'), api<Channel[]>('/api/channels'), api<Environment>('/api/environment'), api<{ summaries: SidebarSummary[] }>('/api/sidebar/summary')]); if (version !== directoryRequest.current) return; directoryRequest.current++; setDirectory(data); setChannels(rooms); setEnvironment(local); setSummaries(summary.summaries); setError(''); }
    catch (error) { if (version === directoryRequest.current) setError(message(error)); if (propagate) throw error; }
  }
  useEffect(() => { void load(); }, []);
  usePanelPrefetch(directory !== null, panelPrefetchers);
  useMessageChanged(() => { void api<{ summaries: SidebarSummary[] }>('/api/sidebar/summary').then(result => setSummaries(result.summaries)).catch(() => {}); });
  usePolling(async signal => {
    try {
      const version = directoryRequest.current;
      const [rooms, agents, summary] = await Promise.all([api<Channel[]>('/api/channels'), api<Directory>('/api/agents'), api<{ summaries: SidebarSummary[] }>('/api/sidebar/summary')]);
      if (!signal.aborted && version === directoryRequest.current) setDirectory(current => current && ({ ...agents, agents: agents.agents.map(fresh => { const old = current.agents.find(item => item.id === fresh.id); return old && old.revision > fresh.revision ? old : fresh; }) }));
      if (!signal.aborted) { setActivityError(false); setChannels(current => rooms.map(fresh => { const old=current.find(room=>room.id===fresh.id); return {...fresh, activity:Math.max(old?.activity??0,fresh.activity), read_position:Math.max(old?.read_position??0,fresh.read_position)}; })); }
      if (!signal.aborted) { setSummaries(summary.summaries); void projectState.reload().catch(() => {}); }
    } catch { if (!signal.aborted) setActivityError(true); }
  }, { interval: 2400, immediate: false }, []);
  useEffect(() => {
    const key = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey)) return;
      const name = event.key.toLowerCase();
      if (name === 'b') { event.preventDefault(); if (narrowSidebar) { setNarrowOpen(value => !value); return; } setCollapsedState(value => { try { localStorage.setItem(collapsedKey, value ? '0' : '1'); } catch { /* Applies to this page only. */ } return !value; }); }
      else if (name === '1' || name === '2') { if (document.querySelector('[role="dialog"], [role="alertdialog"], [role="menu"]')) return; event.preventDefault(); setSidebarTab(name === '1' ? 'agents' : 'projects'); }
      else if (name === ',') { event.preventDefault(); setSettings(current => current ?? 'general'); }
    };
    window.addEventListener('keydown', key);
    return () => window.removeEventListener('keydown', key);
  }, [narrowSidebar]);
  async function markRead(room: Channel) {
    const updated = await api<Channel>(`/api/channels/${room.id}/read`, { id: crypto.randomUUID(), position: room.activity });
    setChannels(current => current.map(item => item.id === room.id ? { ...item, activity: Math.max(item.activity, updated.activity), read_position: Math.max(item.read_position, updated.read_position) } : item));
    await projectState.reload();
  }
  const agent = sidebarAgents.find(agent => agent.id === selected);
  const storedChannel = channels.find(channel => channel.id === selectedChannel);
  const sectionProject = projectState.projects.find(project => project.id === storedChannel?.project);
  const section = sectionProject?.sections.find(section => section.id === selectedChannel);
  const channel = storedChannel && section && sectionProject ? {...storedChannel, section_name: section.name, name: `${sectionProject.name} / ${section.name}`} : storedChannel;
  const project = projectState.projects.find(item => item.id === selectedProject);
  const openProject = (id: string) => { setSidebarTab('projects'); navigate({ project: id }); setDialog(null); };
  const openAgent = (id: string) => { setSidebarTab('agents'); navigate({ agent: id }); setDialog(null); };
  const openChannel = (id: string) => { navigate({ room: id }); setDialog(null); };
  const openSection = (id: string) => { setSidebarTab('projects'); void load(true).then(() => openChannel(id)).catch(error => setError(message(error))); };
  const [attentionFocus, setAttentionFocus] = useState<React.ContextType<typeof NeedsYouFocus>>(null);
  const openAttention = (target: TurnDestination, input?: PendingInput) => {
    const scope = target.channel ? `channel:${target.channel}` : target.thread ? `thread:${target.thread}` : target.agent;
    setAttentionFocus({ scope, turn: target.focus, card: input && requestKey(input), consume: () => setAttentionFocus(null) });
    navigate(target.channel ? { room: target.channel } : { agent: target.agent, thread: target.thread ?? undefined });
    setDialog(null);
  };
  const openReport = (agent: string, destination: string, turn: string) =>
    openAttention({ agent, channel: destination === 'direct' ? null : destination, thread: null, turn, focus: turn });
  const attention = useNeedsYouNotifications(openAttention);
  const attentionByAgent = new Map<string, Map<string, PendingInput>>();
  for (const input of attention ?? []) {
    const conversations = attentionByAgent.get(input.agent) ?? new Map<string, PendingInput>();
    const key = input.channel ?? input.thread ?? '';
    if (!conversations.has(key)) conversations.set(key, input);
    attentionByAgent.set(input.agent, conversations);
  }
  const attentionCount = attention
    ? new Set(attention.map(input => input.channel ?? input.thread ?? input.agent)).size
    : summaries.reduce((sum, item) => sum + (item.needs_you ?? 0), 0);
  const openThread = (owner: string, thread: string) => {
    if (owner.includes('@')) { const [remoteAgent, machine] = owner.split('@'); navigate({ machine, remoteAgent }); }
    else navigate({ agent: owner, ...(thread ? { thread } : {}) });
    setDialog(null); focusComposer();
  };
  const invalidRoute = !!directory && (!!route.agent && (!sidebarAgents.some(agent => agent.id === route.agent) || route.agent === guide?.id && !!route.thread) || !!route.room && !channels.some(channel => channel.id === route.room) || !!route.project && !!projectState.root && !project || !!route.machine && remoteMachinesState.loaded && !remoteMachinesState.machines.some(machine => machine.id === route.machine && machine.agents.some(agent => agent.id === route.remoteAgent)));
  useRouteFallback(!route.notFound && !invalidRoute && !!directory && !route.agent && !route.room && !route.project && !route.view && !route.machine, { agent: defaultAgent?.id, settings: route.settings }, navigate);
  const notFound = route.notFound || invalidRoute || routeThread?.missing;
  const settingsName = settingsGroups.flatMap(group => group.items).find(item => item.id === route.settings)?.label;
  const pageTitle = notFound ? 'Page not found' : route.settings ? `Settings · ${settingsName}` : project ? project.name : channel ? `#${channel.section_name ?? channel.name} · ${sectionProject?.name ?? 'Room'}` : route.machine ? remoteMachinesState.machines.find(machine => machine.id === route.machine)?.agents.find(agent => agent.id === route.remoteAgent)?.name : route.view ? route.view === 'routines' ? 'Routines' : route.view === 'workflows' ? 'Workflows' : 'Automation calendar' : agent ? `${route.inbox ? 'Inbox' : routeThread?.thread?.name ?? 'Main chat'} · ${agent.name}` : '';
  useRoutePresentation(route, pageTitle, setSidebarTab, attentionCount);
  const relativeTime = (value: string | null | undefined) => { if (!value) return ''; const age = Math.max(0, Date.now() - Date.parse(value)); if (!Number.isFinite(age)) return ''; const minutes = Math.floor(age / 60000); if (minutes < 60) return `${Math.max(1, minutes)}m`; const hours = Math.floor(minutes / 60); if (hours < 24) return `${hours}h`; return `${Math.floor(hours / 24)}d`; };
  const summaryByAgent = new Map(summaries.map(item => [item.agent, item]));
  const summaryFor = (id: string) => summaryByAgent.get(id);
  const needsYou = (id: string) => attention ? attentionByAgent.get(id)?.size ?? 0 : summaryFor(id)?.needs_you ?? 0;
  const openAgentRow = (id: string) => {
    const input = attentionByAgent.get(id)?.values().next().value;
    if (input) openAttention(input, input);
    else openAgent(id);
  };
  const matches = (name: string, preview = '', role = '') => !query || `${name} ${role} ${preview}`.toLocaleLowerCase().includes(query);
  const retainedAgent = view === 'chat' && !selectedProject && !projectThread && !selectedChannel ? selected : '';
  const hiddenAgents = sidebarAgents.filter(item => item.hidden);
  const matchingHiddenAgents = hiddenAgents.filter(item => !onlyNeedsYou || needsYou(item.id)).filter(item => matches(item.name, summaryFor(item.id)?.preview, item.role));
  const localAgents = sidebarAgents.filter(item => !item.hidden || item.id === retainedAgent).filter(item => !onlyNeedsYou || needsYou(item.id)).filter(item => matches(item.name, summaryFor(item.id)?.preview, item.role));
  const restoreFlight = useRef(false);
  const [restoring, setRestoring] = useState('');
  async function restoreAgent(item: Agent) {
    if (restoreFlight.current) return;
    restoreFlight.current = true; setRestoring(item.id); setAgentActionError('');
    try {
      await api(`/api/agents/${item.id}/visibility`, { id: crypto.randomUUID(), revision: item.visibility_revision ?? 0, hidden: false });
      await load(true);
    } catch (error) { setAgentActionError(message(error)); }
    finally { restoreFlight.current = false; setRestoring(''); }
  }
  const agentRow = (id: string) => {
    const item = sidebarAgents.find(agent => agent.id === id);
    if (!item) return null;
    const current = !notFound && view === 'chat' && !selectedProject && !projectThread && !selectedChannel && item.id === selected, summary = summaryFor(item.id);
    const count = needsYou(item.id), status = count ? 'Needs you' : summary?.status === 'running' ? 'Working' : 'Idle';
    return collapsed
      ? <button key={item.id} type="button" className={`bot-rail-item${current ? ' is-selected' : ''}`} aria-current={current ? 'page' : undefined} onClick={() => openAgentRow(item.id)} title={item.name} aria-label={`${item.name} ${status}`}><AgentAvatar agent={item} size={36} /></button>
      : <AgentActions key={item.id} agent={item} request={api} onChanged={() => load(true)} onInbox={() => { navigate({ agent: item.id, inbox: true }); setDialog(null); setNarrowOpen(false); }} onEdit={() => { openAgent(item.id); if (item.builtin) setGuideConnectionOpen(true); else setDialog('edit'); }} onDelete={() => deleteAgent(item)} onError={error => setAgentActionError(message(error))}><button type="button" className={`bot-row${current ? ' is-selected' : ''}`} aria-label={`${item.name} ${status}${count ? `, ${count}` : ''}${item.provider_available === false ? ' - Provider account unavailable' : ''}`} aria-current={current ? 'page' : undefined} onClick={() => openAgentRow(item.id)}>
        <span className="bot-row__avatar"><AgentAvatar agent={item} size={36} /><span className={`status-dot ${count || item.provider_available === false ? 'is-attention' : summary?.status === 'running' ? 'is-working' : 'is-idle'}`} aria-hidden /></span>
        <span className="bot-row__text"><span className="bot-row__name">{item.name}</span><span className="bot-row__preview" title={plainPreview(summary?.preview) || undefined}>{item.hidden ? 'Hidden' : plainPreview(summary?.preview) || item.role || ''}</span></span>{count ? <NeedsYouBadge count={count} /> : summary?.time && <time className="bot-row__time" dateTime={summary.time}>{relativeTime(summary.time)}</time>}
      </button></AgentActions>;
  };
  const remoteMachines = remoteMachinesState.machines.filter(machine => !onlyNeedsYou && (!onlyOnline || machine.status === 'online')).sort((a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id));
  const anyMatchingAgents = localAgents.length > 0 || matchingHiddenAgents.length > 0 || remoteMachines.some(machine => machine.agents.some(item => matches(item.name, summaryFor(`${item.id}@${machine.id}`)?.preview, item.role)));
  const switcherItems: SwitcherItem[] = [
    ...(agent && !agent.builtin && !route.inbox && !route.room && !route.project && !route.machine && !route.view ? [{ id: 'new-conversation', label: 'New conversation', detail: 'Ctrl/Cmd+Shift+O', icon: <Plus size={16} aria-hidden />, group: 'Create', onSelect: () => { setSettings(null); switcherFrame(() => window.dispatchEvent(new Event('minarbot:new-conversation'))); } }] : []),
    ...sidebarAgents.filter(agent => !agent.hidden).map(agent => ({ id: `agent:${agent.id}`, label: agent.name, icon: <AgentAvatar agent={agent} size={24} />, group: 'Agents', onSelect: () => { setSettings(null); openAgent(agent.id); } })),
    ...hiddenAgents.map(agent => ({ id: `restore:${agent.id}`, label: `Restore ${agent.name}`, group: 'Hidden agents', icon: <AgentAvatar agent={agent} size={24} />, onSelect: () => { setSettings(null); setSidebarTab('agents'); void restoreAgent(agent); } })),
    ...remoteMachines.flatMap(machine => machine.agents.map(agent => ({ id: `remote:${machine.id}:${agent.id}`, label: agent.name, icon: <AgentAvatar agent={agent} size={24} />, detail: machine.name, group: 'Agents', onSelect: () => { setSettings(null); openThread(`${agent.id}@${machine.id}`, ''); } }))),
    ...projectState.projects.map(project => ({ id: `project:${project.id}`, label: project.name, group: 'Projects', onSelect: () => { setSettings(null); openProject(project.id); } })),
    ...channels.filter(channel => !channel.archived).map(channel => ({ id: `channel:${channel.id}`, label: `# ${channel.name}`, detail: projectState.projects.find(project => project.id === channel.project)?.name, group: 'Sections', onSelect: () => { setSettings(null); openSection(channel.id); } })),
    ...projectState.projects.flatMap(project => project.threads.map(thread => ({ id: `thread:${thread.id}`, label: thread.name, detail: project.name, group: 'Conversations', onSelect: () => { setSettings(null); openThread(thread.agent, thread.id); } }))),
    ...availableSettings.flatMap(group => group.items.map(item => ({ id: `settings:${item.id}`, label: item.label, icon: <item.icon size={16} aria-hidden />, group: 'Settings', onSelect: () => setSettings(item.id) }))),
    { id: 'new-agent', label: 'New agent', group: 'Create', onSelect: () => { setSettings(null); setDialog('agent'); } },
    { id: 'new-project', label: 'New project', group: 'Create', onSelect: () => { setSettings(null); setDialog('project'); } },
  ];
  const created = (item: Agent) => {
    setDirectory(current => current && { ...current, agents: [...current.agents.filter(agent => agent.id !== item.id), item] });
    openAgent(item.id);
    void load();
  };
  let content: ReactNode;
  if (error) content = <PageView><div className="empty"><h1>Workspace is not open</h1><p role="alert">{error}</p><button onClick={() => void load()}>Try again</button></div></PageView>;
  else if (!directory) content = <main className="chat chat--empty bs-root"><p className="empty-chat__sub" role="status">Loading workspace...</p></main>;
  else if (notFound) content = <PageView><EmptyState title="Page not found" action={<Button onClick={() => navigate(directory.agents[0] ? { agent: directory.agents[0].id } : {}, true)}>Back to chats</Button>}>This page may have moved or no longer exists.</EmptyState></PageView>;
  else if (deleting) content = <main className="chat chat--empty bs-root"><p className="empty-chat__sub" role="status">Deleting agent...</p></main>;
  else if (view === 'remote') { const machine = remoteMachinesState.machines.find(item => item.id === remoteSelection?.machine); content = <RemoteAgentDetail key={`${machine?.id}:${remoteSelection?.agent}`} request={api} machine={machine} agent={machine?.agents.find(item => item.id === remoteSelection?.agent)} />; }
  else if (view === 'workflows') content = <PageView wide><Suspense fallback={<Delayed><p className="empty" role="status">Loading workflow directory...</p></Delayed>}><WorkflowDirectory agents={directory.agents} request={api} renderConversation={(id, owner) => <WorkflowConversation key={id} agent={directory.agents.find(item => item.id === owner)} threadId={id} configured={directory.configured} onOpenAgent={openThread} onOpenChannel={openChannel} />} /></Suspense></PageView>;
  else if (view === 'calendar') content = <PageView wide><AutomationCalendar projects={projectState.projects} agents={directory.agents} request={api} onThread={openThread} onReport={openReport} /></PageView>;
  else if (view === 'routines') content = <PageView wide><RoutinesHub key={routineAgent} initialAgent={routineAgent} agents={directory.agents.filter(agent => !agent.hidden)} projects={projectState.projects} onOpen={openThread} /></PageView>;
  else if (project) content = <PageView wide><ProjectHome project={project} agents={directory.agents} state={projectState} request={api} onChat={id => { void load(true).then(() => openChannel(id)); }} onThread={openThread} onReport={openReport} onDeleted={() => { navigate({}, true); void load(); }} /></PageView>;
  else if (route.inbox && agent) content = <PageView wide><AgentInbox key={agent.id} agent={agent} request={api} onBack={() => openAgent(agent.id)} onSettings={() => setSettings('email')} /></PageView>;
  else if (route.thread && !routeThread?.thread) content = <PageView>{routeThread?.error ? <Alert tone="error">{routeThread.error} Retrying…</Alert> : <p role="status">Loading conversation...</p>}</PageView>;
  else if (projectThread && directory.agents.find(agent => agent.id === projectThread.agent)) { const owner = directory.agents.find(agent => agent.id === projectThread.agent)!; const thread = projectState.projects.find(project => project.id === projectThread.project)?.threads.find(thread => thread.id === projectThread.thread) ?? projectThread.conversation; content = thread ? <Chat onOpenAgent={openThread} key={thread.id} agent={owner} thread={{...thread,revision:1,archived:0,deleted:0}} configured={directory.configured} menuItems={[{label:'Back to project',onSelect:()=>openProject(projectThread.project)}]} /> : <PageView><EmptyState title="Project conversation unavailable" /></PageView>; }
  else if (channel) content = <Chat onOpenAgent={openThread} key={`channel:${channel.id}`} agent={channel} channel={{ ...channel, lead: directory.agents.find(agent => agent.id === channel.lead)?.hidden ? null : channel.lead }} onRead={() => markRead(channel)} members={directory.agents.filter(agent => channel.members.includes(agent.id) && !agent.hidden)} onEdit={channel.project ? undefined : () => setDialog('edit')} menuItems={channel.project ? [{label:'Back to project',onSelect:()=>openProject(channel.project!)}] : []} configured={directory.configured} />;
  else if (agent?.builtin) content = <Chat key={agent.id} agent={agent} configured={!!agent.runnable} onRestored={() => load(true)}
    onEdit={() => setGuideConnectionOpen(true)} onOpenAgent={openThread} onOpenChannel={openChannel} />;
  else if (agent) content = <AgentConversations attention={attention ? new Set(attentionByAgent.get(agent.id)?.keys()) : undefined} onRestored={() => load(true)} onExport={() => setAgentContent({ agent, kind: 'export' })} onManageRoutines={id => { setRoutineAgent(id); setView('routines'); }} onOpenChannel={openChannel} key={agent.id} agent={agent} firstRun={firstRunAgent === agent.id} workflowAgents={directory.agents} configured={directory.configured} initialThread={route.thread ?? ''} onSelectThread={(thread, replace) => { navigate(current => ({ ...current, agent: agent.id, thread: thread || undefined, result: current.thread === (thread || undefined) ? current.result : undefined }), replace); focusComposer(); }} onOpenAgent={openThread} onEdit={() => setDialog('edit')} />;
  else content = <FirstRun request={api} guide={guide} onGuide={async (item, connection) => { if (!item.runnable) { await saveGuideConnection(api, item, connection); await load(true); } openAgent(item.id); focusComposer(); }} isClientError={error => error instanceof HttpError && error.status < 500} settingsOpen={!!settings} onOpenEngines={() => setSettings('engines')} onCreated={item => { setFirstRunAgent(item.id); created(item); focusComposer(); }} />;
  const providerReady = directory?.configured || guide?.runnable;
  const providerStatus = activityError ? 'Status not updated. Trying again.' : providerReady ? 'Provider configured' : 'Provider not configured';
  const navigation = <aside ref={sidebar.ref} className="sidebar bs-root" style={{ width: `${sidebar.width}px` }} aria-label="Workspace navigation" onClick={event => { if (narrowSidebar && (event.target as HTMLElement).closest('.bot-row, .project-navigation__name, .project-navigation__room')) setNarrowOpen(false); }}>
      <div className="sidebar__titlebar">
        <span className="sidebar__bots-label">Minarbot</span>
        <nav className="sidebar__controls" aria-label="Automation">
          <IconButton label="Routines" aria-current={view === 'routines' ? 'page' : undefined} disabled={!directory} onClick={() => { setRoutineAgent(''); setView('routines'); setDialog(null); }}><CalendarClock size={16} aria-hidden /></IconButton>
          <IconButton label="Workflows" onPointerEnter={WorkflowDirectory.prefetch} onFocus={WorkflowDirectory.prefetch} aria-current={view === 'workflows' ? 'page' : undefined} disabled={!directory} onClick={() => { setView('workflows'); setDialog(null); }}><WorkflowIcon size={16} aria-hidden /></IconButton>
          <IconButton label="Automation calendar" aria-current={view === 'calendar' ? 'page' : undefined} disabled={!directory} onClick={() => { setView('calendar'); setNarrowOpen(false); setDialog(null); }}><CalendarDays size={16} aria-hidden /></IconButton>
          <ActionMenu label="Create new" onCloseAutoFocus={() => {
            if (!importAfterNewMenu.current) return;
            importAfterNewMenu.current = false;
            setWorkspaceDialog('import');
          }} trigger={<IconButton label="Create new"><Plus size={16} aria-hidden /></IconButton>} items={[
            ...(agent && !agent.builtin ? [{ label: 'New conversation', icon: <MessageSquare size={15} aria-hidden />, hint: 'Ctrl/Cmd+Shift+O', onSelect: () => { openAgent(selected); switcherFrame(() => window.dispatchEvent(new Event('minarbot:new-conversation'))); } }] : []),
            { label: 'New agent', icon: <Plus size={15} aria-hidden />, onSelect: () => setDialog('agent'), disabled: !directory },
            { label: 'Import template', prefetch: AgentTemplateImport.prefetch, icon: <Download size={15} aria-hidden />, onSelect: () => { importAfterNewMenu.current = true; }, disabled: !directory },
            { label: 'New project', icon: <FolderOpen size={15} aria-hidden />, onSelect: () => setDialog('project'), disabled: !directory },
            { label: 'Add a machine', icon: <Monitor size={15} aria-hidden />, onSelect: () => setSettings('machines') },
          ]} />
          <IconButton label="Collapse sidebar" aria-keyshortcuts="Control+b Meta+b" onClick={() => setCollapsed(true)}><PanelLeft size={16} aria-hidden /></IconButton>
        </nav>
      </div>
      <div className="sidebar__tabs">
      <Tabs<SidebarTab> label="Sidebar" value={sidebarTab} onChange={setSidebarTab} tabs={[
        { value: 'agents', label: 'Agents', count: sidebarAgents.filter(item => !item.hidden || item.id === retainedAgent).length + remoteMachinesState.machines.reduce((count, machine) => count + machine.agents.length, 0) },
        { value: 'projects', label: 'Projects', count: projectState.projects.length },
      ]}>
      <div className="sidebar__search">
        <Search size={15} aria-hidden /><Input aria-label={sidebarTab === 'agents' ? 'Search agents' : 'Search projects'} placeholder={sidebarTab === 'agents' ? 'Search agents' : 'Search projects'} value={search} onChange={event => setSearches(current => ({ ...current, [sidebarTab]: event.target.value }))} />
        {sidebarTab === 'agents' && <DropdownMenu.Root open={filterMenu} onOpenChange={setFilterMenu}>
          <DropdownMenu.Trigger asChild><IconButton label="Filter sidebar" aria-pressed={onlyOnline || onlyNeedsYou}><Filter size={15} aria-hidden /></IconButton></DropdownMenu.Trigger>
          <DropdownMenu.Portal><DropdownMenu.Content className="k-pop k-menu" align="end" sideOffset={6}>
            <DropdownMenu.CheckboxItem className="k-menu__item" checked={onlyOnline} onCheckedChange={setOnlyOnline}><DropdownMenu.ItemIndicator><Check size={14} aria-hidden /></DropdownMenu.ItemIndicator>Only online</DropdownMenu.CheckboxItem>
            <DropdownMenu.CheckboxItem className="k-menu__item" checked={onlyNeedsYou} onCheckedChange={setOnlyNeedsYou}><DropdownMenu.ItemIndicator><Check size={14} aria-hidden /></DropdownMenu.ItemIndicator>Needs you</DropdownMenu.CheckboxItem>
          </DropdownMenu.Content></DropdownMenu.Portal>
        </DropdownMenu.Root>}
      </div>
      {agentActionError && <Alert tone="error" actions={<IconButton label="Dismiss agent error" onClick={() => setAgentActionError('')}><X size={14} aria-hidden /></IconButton>}>{agentActionError}</Alert>}
      <div className="sidebar__scroll">
        <TabPanel value="agents">
        <nav className="machine-groups" aria-label="Machines">
        {(!(query || onlyNeedsYou) || localAgents.length > 0) && <MachineGroup name="This device" subtitle={environment?.name} count={sidebarAgents.filter(item => !item.hidden || item.id === retainedAgent).length} expanded={query ? searchMachineGroups.local !== false : machineGroups.local !== false} onExpandedChange={expanded => setMachineExpanded('local', expanded)} local>
          <div className="bot-list sidebar-agents">
            <nav className="bot-list" aria-label="Choose agent">{localAgents.map(item => agentRow(item.id))}
            {directory && !directory.agents.length && <button type="button" className="bot-list__empty" onClick={() => setDialog('agent')}><span className="bot-list__empty-plus"><Plus size={14} aria-hidden /></span><span><span className="bot-row__name">New agent</span></span></button>}
            </nav>
          </div>

        </MachineGroup>}
        {remoteMachines.filter(machine => !query || machine.agents.some(item => matches(item.name, summaryFor(`${item.id}@${machine.id}`)?.preview, item.role))).map(machine => <MachineGroup key={machine.id} name={machine.name} subtitle={`${machine.name} · ${machineStatus[machine.status]}`} count={machine.agentCount} status={machine.status} expanded={query ? searchMachineGroups[machine.id] !== false : machineGroups[machine.id] !== false} onExpandedChange={expanded => setMachineExpanded(machine.id, expanded)}>
          {machine.status !== 'online' && <p className="machine-group__note">{machineStatus[machine.status]} · Last known agents</p>}
          <nav className="bot-list" aria-label={`Agents on ${machine.name}`}>{machine.agents.filter(item => matches(item.name, summaryFor(`${item.id}@${machine.id}`)?.preview, item.role)).map(item => {
              const current = view === 'remote' && remoteSelection?.machine === machine.id && remoteSelection.agent === item.id, summary = summaryFor(`${item.id}@${machine.id}`);
              return <button key={item.id} type="button" className={`bot-row${current ? ' is-selected' : ''}`} aria-label={`${item.name} on ${machine.name}`} aria-current={current ? 'page' : undefined} onClick={() => { navigate({ machine: machine.id, remoteAgent: item.id }); setDialog(null); }}>
                <span className="bot-row__avatar"><AgentAvatar agent={item} size={36} /><span className={`status-dot${machine.status === 'online' ? ' is-online' : ' is-offline'}`} role="img" aria-label={machineStatus[machine.status]} /></span>
                <span className="bot-row__text"><span className="bot-row__name">{item.name}</span><span className="bot-row__preview" title={plainPreview(summary?.preview) || undefined}>{plainPreview(summary?.preview) || item.role || ''}</span></span>{summary?.time && <time className="bot-row__time" dateTime={summary.time}>{relativeTime(summary.time)}</time>}
              </button>;
            })}
            {!machine.agents.length && <p className="machine-group__note">No agents cached yet.</p>}
          </nav>
        </MachineGroup>)}
        {(query || onlyNeedsYou) && !anyMatchingAgents && <p className="machine-group__note" role="status">{onlyNeedsYou && !query ? 'No agents need you.' : 'No matching agents.'}</p>}
        {remoteMachinesState.error && <p className="machine-group__note" role="status">Machines could not be refreshed.</p>}
        {!onlyNeedsYou && !remoteMachinesState.machines.length && <button type="button" className="bot-list__empty" onClick={() => setSettings('machines')}><Monitor size={16} aria-hidden /><span><span className="bot-row__name">Pair a machine</span></span></button>}
        <HiddenAgents agents={matchingHiddenAgents} searching={!!query} renderAgent={id => {
          const item = hiddenAgents.find(agent => agent.id === id)!;
          return <div className="hidden-agent-row"><button type="button" className="bot-row" aria-label={`Open hidden agent ${item.name}`} onClick={() => openAgent(item.id)}><AgentAvatar agent={item} size={36} /><span className="bot-row__text"><span className="bot-row__name">{item.name}</span></span></button><Button size="sm" variant="ghost" aria-label={`Restore ${item.name}`} busy={restoring === id} disabled={!!restoring} onClick={() => void restoreAgent(item)}>Restore</Button></div>;
        }} />
        </nav>
        </TabPanel>
        <TabPanel value="projects">
          <ProjectSidebar state={projectState} selectedProject={selectedProject} selectedSection={view === 'chat' ? selectedChannel : ''} search={query} onProject={openProject} onSection={openSection} onNew={() => setDialog('project')} onArchived={id => { if (selectedProject === id || channel?.project === id || projectThread?.project === id) openAgent(selected); void load(); }} />
        </TabPanel>
      </div>
      </Tabs>
      </div>
      <SidebarUpdateCapsule updates={updates} />
      <div className="sidebar__footer">
        <div className="sidebar__fade" />
        <Tooltip content={providerStatus}><button type="button" className="provider-status" aria-label={providerStatus} onClick={() => setSettings('providers')}><i className={providerReady && !activityError ? 'is-on' : ''} aria-hidden /></button></Tooltip>
        <IconButton label="Quick switcher (Ctrl/Cmd+K)" onClick={() => switcher.setOpen(true)}><Search size={16} aria-hidden /></IconButton>
        <IconButton label="Help" onClick={() => setWorkspaceDialog('help')}><BookOpen size={16} aria-hidden /></IconButton>
        <RadixDialog.Trigger asChild><IconButton label="Settings" onPointerEnter={GeneralSettings.prefetch} onFocus={GeneralSettings.prefetch} className="sidebar-settings" onClick={() => { setSettings('general'); setNarrowOpen(false); }}><Settings size={16} aria-hidden /></IconButton></RadixDialog.Trigger>
      </div>
      <PanelResizeHandle panel={sidebar} />
    </aside>;
  return <NeedsYouFocus.Provider value={attentionFocus}><RadixDialog.Root modal open={!!settings} onOpenChange={open => setSettings(current => open ? current ?? 'general' : null)}><div className={`app bs-root${collapsed ? ' is-sidebar-collapsed' : ''}`}>
    {collapsed && <IconButton className="sidebar-reveal icon-btn--bordered" onClick={() => setCollapsed(false)} label="Expand sidebar" tooltip="Sidebar (Ctrl/Cmd+B)"><PanelLeft size={16} aria-hidden /></IconButton>}
    {narrowSidebar ? <RadixDialog.Root open={!collapsed} onOpenChange={setNarrowOpen}><RadixDialog.Portal>
      <RadixDialog.Overlay className="sidebar-backdrop" />
      <RadixDialog.Content asChild aria-describedby={undefined} onOpenAutoFocus={event => { event.preventDefault(); (event.currentTarget as HTMLElement).focus(); }} onCloseAutoFocus={event => { event.preventDefault(); document.querySelector<HTMLButtonElement>('.sidebar-reveal')?.focus(); }}>
        {navigation}
      </RadixDialog.Content>
      <RadixDialog.Title className="sr-only">Workspace navigation</RadixDialog.Title>
    </RadixDialog.Portal></RadixDialog.Root> : !collapsed && navigation}
    {content}
    {switcher.open && <QuickSwitcher items={switcherItems} agents={directory?.agents ?? []} onClose={() => switcher.setOpen(false)} onChoose={item => { switcher.setOpen(false); switcherFrame(() => { item.onSelect(); if (['Agents', 'Conversations', 'Sections'].includes(item.group)) switcherFrame(focusComposer); }); }} onThread={(agent, thread) => { setSettings(null); openThread(agent, thread); }} />}
    {dialog === 'agent' && directory && <AgentForm request={api} isClientError={error => error instanceof HttpError && error.status < 500} model={directory.defaultModel} onCancel={() => setDialog(null)} onCreated={item => { created(item); setDialog(null); }} />}
    {dialog === 'project' && directory && <CreateProject state={projectState} agents={directory.agents} request={api} onClose={() => setDialog(null)} onCreated={item => { openSection(item.channel); }} />}
    {dialog === 'edit' && directory && channel && <ChannelForm key={`${channel.id}:${channel.revision}:${editReload}`} agents={directory.agents} channel={channel} onCancel={() => setDialog(null)} onReload={() => { void load().then(() => setEditReload(value => value + 1)); }} onCreated={() => { void load(); setDialog(null); }} />}
    {dialog === 'edit' && directory && !channel && agent && <AgentForm request={api} isClientError={error => error instanceof HttpError && error.status < 500} key={`${agent.id}:${editReload}`} model={directory.defaultModel} profile={agent} onCancel={() => setDialog(null)} onDelete={item => deleteAgent(item)} onVisibilityChanged={() => load(true)} onReload={() => load(true).then(() => setEditReload(value => value + 1))} onCreated={() => { void load(); setDialog(null); }} />}
    {guideConnectionOpen && guide && <GuideConnectionDialog agent={guide} request={api}
      onSaved={async () => { await load(true); setGuideConnectionOpen(false); focusComposer(); }} onClose={() => setGuideConnectionOpen(false)}
      onProviders={() => { setGuideConnectionOpen(false); setSettings('providers'); }}
      onEngines={() => { setGuideConnectionOpen(false); setSettings('engines'); }} />}
    {workspaceDialog && <Dialog open size="lg" title={workspaceDialog === 'help' ? 'Keyboard shortcuts' : 'Import template'} onOpenChange={open => { if (!open) setWorkspaceDialog(null); }}><div className="legacy"><Suspense fallback={<Delayed><p role="status">Loading...</p></Delayed>}>{workspaceDialog === 'help' ? <HelpPage /> : <AgentTemplateImport request={api} onClose={() => setWorkspaceDialog(null)} onCreated={item => { created(item); setWorkspaceDialog(null); }} />}</Suspense></div></Dialog>}
    {agentContent && <Dialog open size="lg" title={`${agentContent.agent.name} · Export template`} onOpenChange={open => { if (!open) setAgentContent(null); }}><div className="legacy"><Suspense fallback={<Delayed><p role="status">Loading...</p></Delayed>}><AgentTemplateExport id={agentContent.agent.id} request={api} /></Suspense></div></Dialog>}
    <RadixDialog.Portal><RadixDialog.Content className="app-settings bs-root" aria-describedby={undefined} onEscapeKeyDown={event => { if (settingsQuery && document.activeElement === settingsSearch.current) { event.preventDefault(); setSettingsQuery(''); } }} onOpenAutoFocus={event => {
      event.preventDefault();
      const page = (event.currentTarget as HTMLElement).querySelector<HTMLElement>('.app-settings__page');
      (page?.querySelector<HTMLElement>('h1[tabindex], button:not(:disabled), input:not([type="hidden"]):not(:disabled), [tabindex="0"]') ?? page)?.focus();
    }}>
      <aside className="app-settings__nav">
        <RadixDialog.Title asChild><div className="app-settings__heading">Settings</div></RadixDialog.Title>
        <div className="app-settings__search"><Search size={14} aria-hidden /><input ref={settingsSearch} type="search" aria-label="Search settings" aria-keyshortcuts="/" placeholder="Search" value={settingsQuery} onChange={event => setSettingsQuery(event.target.value)} onKeyDown={event => {
          if (event.key === 'Escape' && settingsQuery) { event.preventDefault(); event.stopPropagation(); setSettingsQuery(''); }
          if (event.key === 'ArrowDown' || event.key === 'Enter') { event.preventDefault(); event.currentTarget.closest('aside')?.querySelector<HTMLButtonElement>('nav button')?.focus(); }
        }} /><kbd aria-hidden>/</kbd></div>
        <nav aria-label="Settings" className="app-settings__groups" onKeyDown={event => {
          if (!(event.target instanceof HTMLButtonElement)) return;
          const buttons = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>('button'));
          const index = buttons.indexOf(event.target);
          let next: number;
          if (event.key === 'ArrowDown') next = (index + 1) % buttons.length;
          else if (event.key === 'ArrowUp') next = (index - 1 + buttons.length) % buttons.length;
          else if (event.key === 'Home') next = 0;
          else if (event.key === 'End') next = buttons.length - 1;
          else return;
          event.preventDefault(); buttons[next]?.focus();
        }}>{filteredSettings.map(group => <div key={group.label} role="group" aria-labelledby={`settings-${group.label}`} className="app-settings__group"><h2 id={`settings-${group.label}`}>{group.label}</h2>{group.items.map(section => <Tooltip key={section.id} content={section.label} side="right" sideOffset={8}><button type="button" aria-label={section.label} onPointerEnter={settingsPrefetch[section.id]} onFocus={settingsPrefetch[section.id]} className={`cfg-nav__item${settings === section.id ? ' is-active' : ''}`} aria-current={settings === section.id ? 'page' : undefined} onClick={event => { if (settings === section.id) event.currentTarget.closest('[role="dialog"]')?.querySelector<HTMLElement>('.app-settings__page h1')?.focus(); else setSettings(section.id); }}><section.icon size={16} aria-hidden /><span>{section.label}</span></button></Tooltip>)}</div>)}{!filteredSettings.length && <p className="app-settings__no-results" role="status">No settings found</p>}</nav>
      </aside>
      <IconButton className="app-settings__close" label="Close settings" onClick={() => setSettings(null)}><X size={16} aria-hidden /></IconButton>
      <div className="app-settings__main">
        <div className={`app-settings__page legacy${settings === 'general' || settings === 'appearance' ? ' app-settings__page--preferences' : ''}`} tabIndex={-1}>
          {!directory ? <p className="empty" role="status">Loading workspace...</p>
            : settings === 'skills' ? <Suspense fallback={<Delayed><p className="empty" role="status">Loading skill library...</p></Delayed>}><SkillsPanel agents={directory.agents} request={api} onClose={() => setSettings(null)} /></Suspense>
            : settings === 'providers' ? <ProvidersPanel request={api} />
            : settings === 'engines' ? <EnginesPanel request={api} />
            : settings === 'devices' ? <DevicesPanel request={api} />
            : settings === 'computer' ? <Suspense fallback={<Delayed><p className="empty" role="status">Loading Computer...</p></Delayed>}><ComputerSetupPage request={api} /></Suspense>
            : settings === 'updates' ? <UpdatesPanel updates={updates} />
            : settings === 'machines' ? <MachinesPanel request={api} onChanged={() => { void remoteMachinesState.reload(); void api<Environment>('/api/environment').then(setEnvironment).catch(() => setActivityError(true)); }} />
            : settings === 'hub' ? <Suspense fallback={<Delayed><p className="empty" role="status">Loading hub connection...</p></Delayed>}><HubConnection agents={directory.agents} request={api} /></Suspense>
            : settings === 'email' ? <AgentEmailSettings request={api} />
            : settings === 'connected-apps' ? <Suspense fallback={<Delayed><p className="empty" role="status">Loading connected apps...</p></Delayed>}><ConnectedAppsPanel agents={directory.agents.filter(agent => !agent.hidden)} sections={channels.filter(channel => !channel.archived && channel.project)} request={api} /></Suspense>
            : settings === 'mcp-server' ? <Suspense fallback={<Delayed><p className="empty" role="status">Loading MCP server...</p></Delayed>}><McpServerPanel request={api} /></Suspense>
            : settings === 'mcp' ? <Suspense fallback={<Delayed><p className="empty" role="status">Loading MCP connections...</p></Delayed>}><McpPanel agents={directory.agents} request={api} onClose={() => setSettings(null)} /></Suspense>
            : settings === 'vault' ? <Suspense fallback={<Delayed><p className="empty" role="status">Loading Vault...</p></Delayed>}><VaultSettings agents={directory.agents} request={api} /></Suspense>
            : settings === 'data' ? <Suspense fallback={<Delayed><p className="empty" role="status">Loading shared data...</p></Delayed>}><SharedDataPanel agents={directory.agents} request={api} onClose={() => setSettings(null)} /></Suspense>
            : settings === 'usage' ? <Suspense fallback={<Delayed><p className="empty" role="status">Loading usage...</p></Delayed>}><UsagePanel request={api} onClose={() => setSettings(null)} /></Suspense>
            : settings === 'backup' ? <Suspense fallback={<Delayed><p className="empty" role="status">Loading backup...</p></Delayed>}><BackupPanel onClose={() => setSettings(null)} /></Suspense>
            : settings === 'general' ? <Suspense fallback={<Delayed><p className="empty" role="status">Loading general settings...</p></Delayed>}><GeneralSettings request={api} notificationsMuted={notificationsMuted} notificationError={notificationError} onNotificationsChange={enabled => { try { localStorage.setItem(notificationMuteKey, enabled ? '0' : '1'); setNotificationsMuted(!enabled); setNotificationError(''); } catch { setNotificationError('Notification preference could not be saved. Check browser storage.'); } }} /></Suspense>
            : <Suspense fallback={<Delayed><p className="empty" role="status">Loading appearance settings...</p></Delayed>}><PresentationSettings /></Suspense>}
        </div>
      </div>
    </RadixDialog.Content></RadixDialog.Portal>
    <UpdateEverythingDialog updates={updates} />
  </div></RadixDialog.Root></NeedsYouFocus.Provider>;
}

function ComputerWindow() {
  const agent = new URLSearchParams(location.search).get('computer') ?? '';
  const [name, setName] = useState('Agent'), [error, setError] = useState('');
  useRoutePresentation({}, error ? 'Computer unavailable' : `Computer · ${name}`);
  useEffect(() => {
    let active = true;
    void api<Directory>('/api/agents').then(data => { const found = data.agents.find(item => item.id === agent); if (active) { if (found) setName(found.name); else setError('Agent not found.'); } }).catch(error => { if (active) setError(message(error)); });
    return () => { active = false; };
  }, [agent]);
  return <main className="computer-window bs-root">{error ? <Alert tone="error">{error}</Alert> : <ComputerViewer agent={agent} name={name} request={api} popout />}</main>;
}
applyPresentation(document.documentElement, readPresentation().value);
window.addEventListener('storage', event => {
  if (event.key !== presentationKey && event.key !== null) return;
  try { if (event.storageArea !== localStorage) return; } catch { return; }
  applyPresentation(document.documentElement, readPresentation().value);
});
createRoot(document.getElementById('root')!).render(<ConfirmProvider>{new URLSearchParams(location.search).has('computer') ? <ComputerWindow /> : <App />}</ConfirmProvider>);

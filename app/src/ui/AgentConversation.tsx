import { useState, type ReactNode } from 'react';
import { ChevronDown, CircleAlert } from 'lucide-react';
import { Collapsible as Disclosure } from 'radix-ui';
import { AgentAvatar } from '../AgentAvatar';
import { Markdown, StreamingMarkdown } from '../Markdown';
import { HtmlReplies } from '../HtmlReply';
import { McpAppReplies, type McpAppHandler } from '../McpAppFrame';
import type { McpAppReference } from '../mcp-app-host';
import type { HtmlReplyAction, HtmlReplyReference } from '../html-reply';
import { AgentMessage } from './ChatPrimitives';
import { Collapsible } from './kit';
import { activityLabel, livePartial, liveTurn, turnActivitySteps, type TurnActivityData } from './transcript';
import { TurnActivity } from './TurnActivity';
import { useLiveClock } from '../hooks/useLiveClock';
import type { StreamPartial } from '../../shared/turn-stream';

export type Exchange = { id: string; agent: string; name: string; role?: string; machine?: string; thread: string; task?: string; reply: string | null; detailReply?: string | null; error: string | null; status: string; paused: number; truncated: boolean; activity?: TurnActivityData; created?: string; generatedFiles?: { id: string; name: string; size: number; thread: string; deleted?: boolean }[] };
export function AgentConversation({ items, sender, scope, onOpen }: { items: Exchange[]; sender: string; scope: string; onOpen?: (agent: string, thread: string) => void }) {
  return <section className="inline-delegation" aria-label="Agent conversation">{items.map(item => <div className="inline-delegation__exchange" key={item.id}>
    <div className="inline-delegation__request"><span className="exchange-label"><strong>{sender}</strong><span>to {item.name}{item.machine ? ` @ ${item.machine}` : ''}</span></span><p className="message-plain">{item.task ?? 'Shared task'}</p></div>
    <div className="inline-delegation__reply"><AgentAvatar agent={{ id: item.agent, name: item.name }} size={24} /><div className="message-column"><div className="agent-identity"><strong>{item.name}</strong>{item.machine && <span className="message-role">{item.machine}</span>}{item.role && <span className="message-role">{item.role}</span>}</div>
      {item.reply ? <div className="legacy"><Markdown results files={item.generatedFiles} text={item.reply ?? ''} scope={scope} origin={window.location.origin} /></div> : <p className={item.error ? 'message-error' : 'message-muted'}>{item.error ?? (item.paused ? 'Paused' : item.status === 'done' ? 'No reply' : item.status === 'running' ? 'Replying...' : item.status === 'queued' ? 'Queued' : item.status)}</p>}
      {!item.reply && !['queued', 'running'].includes(item.status) && !!item.generatedFiles?.length && <div className="legacy"><Markdown results text="" files={item.generatedFiles} scope={scope} origin={window.location.origin} /></div>}
      <Collapsible title="Exchange details">{item.activity && <TurnActivity turn={{ status: item.status, paused: item.paused, created: item.created ?? '', activity: item.activity }} agentName={item.name} onOpenTask={() => onOpen?.(item.agent, item.thread)} remote={!!item.machine} />}{item.truncated && <div className="legacy"><Markdown results text={item.detailReply ?? item.reply ?? ''} scope={scope} origin={window.location.origin} /></div>}{onOpen && <button type="button" className="link-btn" onClick={() => onOpen(item.agent, item.thread)}>Open conversation with {item.name}</button>}</Collapsible>
    </div></div>
  </div>)}</section>;
}
export type RoomView = { root: string; generation: string; active: boolean; stopped: boolean; round: number; silent: string | null; note: string | null; quiet: { agent: string; name: string; silent: string }[]; messages: { seq: number; turn: string; agent: string; name: string; role: string; target_name?: string; text: string; kind: string }[] };
type RoomTurn = { mcpApps?: McpAppReference[]; htmlReplies?: HtmlReplyReference[]; generatedFiles?: { id: string; name: string; size: number; deleted?: boolean }[]; id: string; agent: string; status: string; paused: number; created: string; error: string | null; partial?: StreamPartial | null; activity?: TurnActivityData; questions?: { id: string; prompt: string; answer: string | null }[] };
type RoomMember = { id: string; name: string; role: string; avatar_revision?: number };
function RoomReply({ turn, member, text, scope, details, controls, extras, onHtmlReplyAction, onMcpAppAction }: { onMcpAppAction?: McpAppHandler; turn: RoomTurn; onHtmlReplyAction?: (action: HtmlReplyAction) => void; member: RoomMember; text?: string; scope: string; details: ReactNode; controls: ReactNode; extras: ReactNode }) {
  const [open, setOpen] = useState(false);
  const live = liveTurn(turn), streaming = livePartial(turn), now = useLiveClock(live);
  const answer = text ?? turn.partial?.text ?? '';
  const hasWork = turnActivitySteps(turn, member.name).length > 0;
  const duration = <>{live && turn.status !== 'queued' && <span className="activity-working" aria-hidden />}{activityLabel(turn, now)}{turn.activity?.fallback_model && <CircleAlert size={12} role="img" aria-label="Fallback model used" />}</>;
  return <Disclosure.Root open={open} onOpenChange={setOpen} asChild><section className="room-agent" aria-label={`Reply from ${member.name}`}>
    <div className={`${text ? 'room-agent__line' : 'room-pending'}${live ? ' is-working' : ''}`}>
      <AgentAvatar agent={member} size={20} /><strong className="room-agent__name message-name" title={member.name}>{member.name}</strong>
      {hasWork ? <Disclosure.Trigger className="room-agent__status" aria-label={activityLabel(turn, now)}>{duration}<ChevronDown size={14} aria-hidden className="k-collapsible__chev" /></Disclosure.Trigger> : <span className="room-agent__status">{duration}</span>}
    </div>
    {hasWork && <Disclosure.Content className="room-agent__activity">{details}</Disclosure.Content>}
    <HtmlReplies replies={turn.htmlReplies} scope={scope} onAction={onHtmlReplyAction} />
    <McpAppReplies apps={turn.mcpApps} scope={scope} onAction={onMcpAppAction} />
    {(answer || !['queued', 'running'].includes(turn.status) && !!turn.generatedFiles?.length) && <div className="message-body" aria-label={streaming ? `Streaming answer from ${member.name}` : `Answer from ${member.name}`}><div className="legacy"><StreamingMarkdown files={turn.generatedFiles} key={turn.id} text={answer} scope={scope} origin={window.location.origin} /></div></div>}
    {extras}
    {turn.error && turn.status !== 'cancelled' && <p className="message-error">{turn.error}</p>}
    <div className="turn-controls" role="group" aria-label="Message actions">{controls}</div>
  </section></Disclosure.Root>;
}
export function RoomMessages({ room, members, pending, turns, scope, details, controls, extras, onHtmlReplyAction, onMcpAppAction }: { onMcpAppAction?: McpAppHandler; room: RoomView; onHtmlReplyAction?: (action: HtmlReplyAction) => void; members: RoomMember[]; pending: RoomTurn[]; turns: RoomTurn[]; scope: string; details: (turn: string) => ReactNode; controls: (turn: string) => ReactNode; extras: (turn: string) => ReactNode }) {
  const rows = [...room.messages.map(item => ({ key: `${item.kind}-${item.turn}`, item, turn: turns.find(turn => turn.id === item.turn) })), ...pending.filter(turn => turn.status !== 'cancelled' || !!turn.activity?.calls.length || !!turn.partial?.text || !!turn.activity?.stream?.length || !!turn.activity?.tools.length || !!turn.activity?.harness?.length).map(turn => ({ key: `reply-${turn.id}`, item: undefined, turn }))];
  return <div className="room-conversation" aria-label="Section conversation">
    {rows.map(({ key, item, turn }) => item?.kind === 'request' ? <AgentMessage key={key} identity={<div className="agent-identity"><AgentAvatar agent={members.find(member => member.id === item.agent) ?? { id: item.agent, name: item.name }} size={24} /><strong className="message-name">{item.name}</strong>{item.target_name && <span className="message-role">to {item.target_name}</span>}</div>}><div className="room-message-request legacy"><Markdown results text={item.text} scope={scope} origin={window.location.origin} /></div></AgentMessage>
      : <RoomReply key={key} onHtmlReplyAction={onHtmlReplyAction} onMcpAppAction={onMcpAppAction} turn={turn ?? { id: item!.turn, agent: item!.agent, status: 'done', paused: 0, created: '', error: null }} member={members.find(member => member.id === (turn?.agent ?? item?.agent)) ?? { id: item?.agent ?? turn!.agent, name: item?.name ?? turn!.agent, role: '' }} text={item?.text} scope={scope} details={details(turn?.id ?? item!.turn)} controls={controls(turn?.id ?? item!.turn)} extras={extras(turn?.id ?? item!.turn)} />)}
    {room.note && <p className="room-system-note" role="status">{room.note}</p>}
  </div>;
}

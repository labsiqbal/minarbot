import { validHtmlReplyContext, type HtmlReplyContext } from './html-reply.ts';

export type ChatCommand = { id: string; text: string; htmlContext?: HtmlReplyContext; files?: string[]; recipient?: string; automatic?: boolean; dispatchMode?: 'queue' | 'steer' | 'restart'; targetTurn?: string };
export type ChannelCommand = { id: string; name: string; instructions: string; members: string[]; lead: string | null; revision?: number; archived?: boolean };
const uuid = (value: unknown): value is string => typeof value === 'string' && /^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/i.test(value);
const text = (value: unknown, max: number) => typeof value === 'string' && !!value.trim() && value.length <= max;

export function restoreChatCommand(raw: string | null, channel: boolean): ChatCommand | null {
  if (raw === null) return null;
  const value = JSON.parse(raw);
  if (!value || Array.isArray(value) || Object.keys(value).some(key => !['id', 'text', 'files', 'recipient', 'automatic', 'dispatchMode', 'targetTurn', 'htmlContext'].includes(key)) || !uuid(value.id) || !(text(value.text, 16000) || typeof value.text === 'string' && value.text.length <= 16000 && !value.text.trim() && Array.isArray(value.files) && value.files.length > 0)
    || value.dispatchMode !== undefined && !['queue', 'steer', 'restart'].includes(value.dispatchMode)
    || value.targetTurn !== undefined && (!uuid(value.targetTurn) || !['steer', 'restart'].includes(value.dispatchMode))
    || value.htmlContext !== undefined && (!validHtmlReplyContext(value.htmlContext) || value.dispatchMode !== undefined && value.dispatchMode !== 'queue')
    || value.files !== undefined && (!Array.isArray(value.files) || value.files.length > 4 || !value.files.every(uuid))
    || (channel ? !((uuid(value.recipient) && value.automatic === undefined) || (value.automatic === true && value.recipient === undefined) || (value.automatic === undefined && value.recipient === undefined)) : value.recipient !== undefined || value.automatic !== undefined)) throw new Error('Invalid message recovery data.');
  return value;
}

export function restoreChannelCommand(raw: string | null, editing: boolean): ChannelCommand | null {
  if (raw === null) return null;
  const value = JSON.parse(raw);
  if (!value || Array.isArray(value) || Object.keys(value).some(key => !['id', 'name', 'instructions', 'members', 'lead', 'revision', 'archived'].includes(key)) || !uuid(value.id) || !text(value.name, 80) || !text(value.instructions, 8000)
    || !Array.isArray(value.members) || !value.members.length || !value.members.every(uuid) || (value.lead !== null && !value.members.includes(value.lead))
    || (editing ? !Number.isSafeInteger(value.revision) || value.revision < 1 || typeof value.archived !== 'boolean' : value.revision !== undefined || value.archived !== undefined)) throw new Error('Invalid channel recovery data.');
  return value;
}

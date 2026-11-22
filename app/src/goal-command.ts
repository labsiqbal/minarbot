export type ModeDraft = { mode: 'goal' | 'loop'; text: string; intervalSeconds?: number };
type GoalBody = { id: string; target: string; maxTurns: number; maxMinutes: number; maxTokens: number; recipient?: string; files?: string[] };
type LoopBody = { id: string; prompt: string; intervalSeconds: number; until?: string; maxRuns: number; recipient?: string };
export type ModeCommand = { mode: 'goal'; body: GoalBody } | { mode: 'loop'; body: LoopBody };

export function parseModeDraft(value: string): ModeDraft | null {
  const goal = /^\/goal(?:\s+([\s\S]*))?$/.exec(value.trim());
  if (goal) return { mode: 'goal', text: goal[1] ?? '' };
  const loop = /^\/loop(?:\s+([\s\S]*))?$/.exec(value.trim());
  if (!loop) return null;
  const interval = /^(\d+)(m|h|d)?(?:\s+([\s\S]*))?$/.exec(loop[1] ?? '');
  if (!interval) throw new Error('Enter a loop interval such as 5m, 2h, or 1d.');
  const seconds = Number(interval[1]) * (interval[2] === 'd' ? 86400 : interval[2] === 'h' ? 3600 : 60);
  if (!Number.isSafeInteger(seconds) || seconds < 60 || seconds > 31536000) throw new Error('Use a loop interval from 1m to 365d.');
  return { mode: 'loop', text: interval[3] ?? '', intervalSeconds: seconds };
}

const uuid = (value: unknown): value is string => typeof value === 'string' && /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(value);
const text = (value: unknown, limit: number): value is string => typeof value === 'string' && !!value.trim() && value.length <= limit;
const number = (value: unknown, max: number, min = 1) => typeof value === 'number' && Number.isSafeInteger(value) && value >= min && value <= max;

export function restoreModeCommand(raw: string | null): ModeCommand | null {
  if (raw === null) return null;
  const value = JSON.parse(raw);
  const body = value?.body;
  if (!uuid(body?.id) || body.recipient !== undefined && !uuid(body.recipient)) throw new Error('Invalid command ID.');
  const common = ['id', 'recipient'];
  if (value.mode === 'goal' && Object.keys(body).every(key => [...common, 'target', 'maxTurns', 'maxMinutes', 'maxTokens', 'files'].includes(key))
    && text(body.target, 12000) && number(body.maxTurns, 100) && number(body.maxMinutes, 1440) && number(body.maxTokens, 10000000)
    && (body.files === undefined || Array.isArray(body.files) && body.files.length <= 4 && body.files.every(uuid))) return value;
  if (value.mode === 'loop' && Object.keys(body).every(key => [...common, 'prompt', 'intervalSeconds', 'until', 'maxRuns'].includes(key))
    && text(body.prompt, 12000) && number(body.intervalSeconds, 31536000, 60) && number(body.maxRuns, 1000)
    && (body.until === undefined || text(body.until, 4000))) return value;
  throw new Error('Invalid goal or loop request.');
}

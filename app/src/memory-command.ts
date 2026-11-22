export type MemoryCommand = { id: string; entry: string; revision: number; text: string; essential: boolean; remove: boolean; automatic?: boolean; restore?: boolean };
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
export function memoryCommand(value: unknown): MemoryCommand {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Invalid memory change.');
  const v = value as MemoryCommand;
  if (Object.keys(v).some(k => !['id', 'entry', 'revision', 'text', 'essential', 'remove', 'automatic', 'restore'].includes(k)) || (v.automatic !== undefined && typeof v.automatic !== 'boolean') ||
    typeof v.id !== 'string' || !uuid.test(v.id) || typeof v.entry !== 'string' || !uuid.test(v.entry) || !Number.isSafeInteger(v.revision) || v.revision < 0 ||
    typeof v.text !== 'string' || v.text.length > 2000 || typeof v.essential !== 'boolean' || typeof v.remove !== 'boolean' || (v.restore !== undefined && typeof v.restore !== 'boolean') || (v.remove && v.restore) ||
    ((v.remove || v.restore) ? v.revision < 1 || v.text !== '' || v.essential : !v.text.trim())) throw new Error('Invalid memory change.');
  return { id: v.id, entry: v.entry, revision: v.revision, text: v.text, essential: v.essential, remove: v.remove, ...(v.restore !== undefined ? { restore: v.restore } : {}), ...(v.automatic !== undefined ? { automatic: v.automatic } : {}) };
}

export type MemoryEntry = { id: string; text: string; essential: number; revision: number; selection_locked?: number };
export type MemoryDraft = { entry: MemoryEntry | null; text: string; essential: boolean; automatic?: boolean };
export function memoryDraft(value: unknown): MemoryDraft {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Invalid memory draft.');
  const v = value as MemoryDraft;
  if (Object.keys(v).length !== (v.automatic === undefined ? 3 : 4) || Object.keys(v).some(k => !['entry', 'text', 'essential', 'automatic'].includes(k)) || (v.automatic !== undefined && typeof v.automatic !== 'boolean') || typeof v.text !== 'string' || v.text.length > 2000 || typeof v.essential !== 'boolean') throw new Error('Invalid memory draft.');
  if (v.entry !== null) {
    const e = v.entry;
    if (!e || typeof e !== 'object' || Array.isArray(e) || Object.keys(e).length !== (e.selection_locked === undefined ? 4 : 5) || Object.keys(e).some(k => !['id', 'text', 'essential', 'revision', 'selection_locked'].includes(k)) || (e.selection_locked !== undefined && ![0, 1].includes(e.selection_locked)) || typeof e.id !== 'string' || !uuid.test(e.id) || typeof e.text !== 'string' || !e.text.trim() || e.text.length > 2000 || ![0, 1].includes(e.essential) || !Number.isSafeInteger(e.revision) || e.revision < 1) throw new Error('Invalid memory draft.');
  }
  return v;
}

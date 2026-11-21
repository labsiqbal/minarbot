export type ComputerControl = { mode: 'agent' | 'user'; who: string; since: string; revision: number };
export type ComputerWait = { turn: string; call_id: string; reason: string; state: string; secret_entry?: string; secret_origin?: string };
export type DesktopStatus = { available: boolean; state: 'off' | 'starting' | 'running' | 'idle'; stopsInMs: number | null; width: number; height: number };

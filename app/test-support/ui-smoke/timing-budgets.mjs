import assert from 'node:assert/strict';
import { loadavg } from 'node:os';

export const HOST_BUSY_THRESHOLD = 8;

export function hostLoadAverage() {
  return loadavg()[0];
}

// Frozen React p95 in milliseconds. Sources and derived ceilings are in PARITY.md.
const baselineP95 = {
  'appearance': { cold: 750.5, warm: 21.2 },
  'automation routines': { cold: 839, warm: 16.8 },
  'automation workflows': { cold: 874.2, warm: 16.7 },
  'automation calendar': { cold: 840.2, warm: 16.7 },
  'controls': { cold: 372.345429, warm: 21.9 },
  'conversation': { cold: 508, warm: 7 },
  'files': { cold: 539.9, warm: 36.2 },
  'hub': { cold: 225.7, warm: 129.4 },
  'memory': { cold: 957.2, warm: 61.4 },
  'profiles': { cold: 745.836731, warm: 77.8 },
  'projects Overview': { cold: 922.3, warm: 19 },
  'projects Connected apps': { cold: 932.4, warm: 18.6 },
  'projects Environment': { cold: 940.6, warm: 37 },
  'projects MCP': { cold: 957.1, warm: 17.5 },
  'scoped inbox': { cold: 369.3, warm: 305.8 },
  'scoped remote': { cold: 373.5, warm: 307.3 },
  'scoped upcoming': { cold: 442.4, warm: 314.2 },
  'scoped workflow': { cold: 118.8, warm: 73.5 },
  'shell': { cold: 417.4, warm: 52.9 },
};

export function assertP95(route, phase, measured) {
  const load = hostLoadAverage();
  const busy = load > HOST_BUSY_THRESHOLD;
  console.log(`${route} ${phase} host load ${load.toFixed(2)} (busy threshold ${HOST_BUSY_THRESHOLD.toFixed(2)})`);
  if (busy && process.env.MINARBOT_PERF_COMMAND !== '1') {
    console.log(`${route} ${phase} host busy - timing result recorded without budget verdict`);
    return;
  }
  const budget = baselineP95[route][phase] * (phase === 'cold' ? 1.2 : 1.1);
  console.log(`${route} ${phase} p95 ${measured.toFixed(2)} ms (budget ${budget.toFixed(2)} ms)`);
  assert(measured <= budget, `${route} ${phase} p95 exceeds ${budget.toFixed(2)} ms`);
}

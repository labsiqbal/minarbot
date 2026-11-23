import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createFrameQueue } from './src/hooks/frame-queue.ts';

test('next-frame work runs once and cleanup cancels every pending callback', t => {
  const pending = new Map<number, FrameRequestCallback>();
  const cancelled: number[] = [];
  let id = 0, calls = 0;
  for (const name of ['requestAnimationFrame', 'cancelAnimationFrame']) {
    const saved = Object.getOwnPropertyDescriptor(globalThis, name);
    Object.defineProperty(globalThis, name, { configurable: true, writable: true, value: () => {} });
    t.after(() => { if (saved) Object.defineProperty(globalThis, name, saved); else Reflect.deleteProperty(globalThis, name); });
  }
  t.mock.method(globalThis, 'requestAnimationFrame', (callback: FrameRequestCallback) => {
    pending.set(++id, callback); return id;
  });
  t.mock.method(globalThis, 'cancelAnimationFrame', (frame: number) => {
    cancelled.push(frame); pending.delete(frame);
  });
  const queue = createFrameQueue();
  queue.schedule(() => { calls++; });
  assert.equal(calls, 0);
  const first = pending.get(1)!; pending.delete(1); first(16);
  assert.equal(calls, 1);
  queue.schedule(() => { calls++; });
  queue.schedule(() => { calls++; });
  queue.cancel(); queue.cancel();
  assert.deepEqual(cancelled, [2, 3]);
  assert.equal(pending.size, 0);
  assert.equal(calls, 1);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import { createQueryCache } from './src/hooks/queryCache.ts';

test('query cache hits synchronously and revalidates the whole path in the background', async () => {
  const cache = createQueryCache();
  const path = '/api/workflows?deleted=0';
  cache.set(path, 1);
  const refresh = cache.refresh(path, async () => 2);
  assert.equal(cache.peek(path)?.data, 1);
  assert.equal(cache.peek('/api/workflows?deleted=1'), undefined);
  await refresh;
  assert.equal(cache.peek(path)?.data, 2);
});

test('invalidation revalidates mounted queries and rejects stale in-flight results', async () => {
  const cache = createQueryCache();
  const path = '/api/routines';
  cache.set(path, 1);
  let resolveOld!: (value: number) => void;
  const old = new Promise<number>(resolve => { resolveOld = resolve; });
  const request = cache.refresh(path, () => old);
  let mountedVersion = cache.version(), updated: Promise<number> | undefined;
  const unmount = cache.subscribe(() => {
    if (mountedVersion === cache.version()) return;
    mountedVersion = cache.version();
    updated = cache.refresh(path, async () => 2);
  });
  cache.invalidate();
  assert.notEqual(cache.peek(path)?.version, cache.version());
  assert.equal(cache.peek(path)?.data, 1);
  assert.equal(await updated, 2);
  resolveOld(1);
  await request;
  assert.equal(cache.peek(path)?.data, 2);
  unmount();
  cache.invalidate();
  assert.notEqual(cache.peek(path)?.version, cache.version());
});

test('a failed revalidation preserves the last successful result', async () => {
  const cache = createQueryCache();
  cache.set('/api/routines', 1);
  await assert.rejects(cache.refresh('/api/routines', async () => { throw new Error('offline'); }), /offline/);
  assert.equal(cache.peek('/api/routines')?.data, 1);
});

test('the cache evicts the least recently used entry at its bound', () => {
  const cache = createQueryCache(2);
  cache.set('/api/one', 1);
  cache.set('/api/two', 2);
  cache.touch('/api/one');
  cache.set('/api/three', 3);
  assert.equal(cache.peek('/api/one')?.data, 1);
  assert.equal(cache.peek('/api/two'), undefined);
  assert.equal(cache.peek('/api/three')?.data, 3);
});

test('clearing the cache discards in-flight results from the previous identity', async () => {
  const cache = createQueryCache();
  const path = '/api/workflows';
  let resolveOld!: (value: number) => void;
  const old = new Promise<number>(resolve => { resolveOld = resolve; });
  const request = cache.refresh(path, () => old);
  cache.clear();
  await cache.refresh(path, async () => 2);
  resolveOld(1);
  await request;
  assert.equal(cache.peek(path)?.data, 2);
});

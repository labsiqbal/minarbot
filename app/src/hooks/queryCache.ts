type Entry = { data: unknown; version: number; read: number };

export function createQueryCache(limit = 100) {
  const entries = new Map<string, Entry>();
  const flights = new Map<string, { version: number; result: Promise<unknown> }>();
  const listeners = new Set<() => void>();
  let version = 0, identity = 0, read = 0;

  const notify = () => { for (const listener of listeners) listener(); };
  function touch(path: string) {
    const entry = entries.get(path);
    if (entry) { entries.delete(path); entries.set(path, entry); }
  }
  function set<T>(path: string, data: T) {
    entries.delete(path);
    entries.set(path, { data, version, read: ++read });
    const oldest = entries.keys().next();
    if (entries.size > limit && !oldest.done) entries.delete(oldest.value);
    notify();
  }
  function refresh<T>(path: string, load: () => Promise<T>): Promise<T> {
    touch(path);
    const pending = flights.get(path);
    if (pending?.version === version) return pending.result as Promise<T>;
    const started = version, owner = identity, previousRead = read;
    const result: Promise<T> = Promise.resolve().then(load).then(data => {
      if (owner !== identity) return data;
      if (started !== version) {
        const latest = entries.get(path);
        if (latest?.version === version && latest.read > previousRead) return latest.data as T;
        const current = flights.get(path);
        return current?.version === version ? current.result as Promise<T> : data;
      }
      set(path, data);
      return data;
    }).finally(() => {
      if (flights.get(path)?.result === result) flights.delete(path);
    });
    flights.set(path, { version: started, result });
    return result;
  }
  return {
    peek: (path: string) => entries.get(path),
    touch, set, refresh,
    version: () => version,
    reads: () => read,
    subscribe(listener: () => void) {
      listeners.add(listener);
      return () => { listeners.delete(listener); };
    },
    invalidate() { version++; notify(); },
    clear() { identity++; version++; entries.clear(); flights.clear(); notify(); },
  };
}

export const queryCache = createQueryCache();

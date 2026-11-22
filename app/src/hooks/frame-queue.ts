export function createFrameQueue() {
  const pending = new Set<number>();
  return {
    schedule(callback: () => void) {
      const frame = requestAnimationFrame(() => { pending.delete(frame); callback(); });
      pending.add(frame);
    },
    cancel() { for (const frame of pending) cancelAnimationFrame(frame); pending.clear(); },
  };
}

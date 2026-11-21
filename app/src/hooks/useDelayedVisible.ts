import { useEffect, useState } from 'react';

export function useDelayedVisible() {
  const [visible, setVisible] = useState(false);
  useEffect(() => {
    const timer = window.setTimeout(() => setVisible(true), 200);
    return () => window.clearTimeout(timer);
  }, []);
  return visible;
}

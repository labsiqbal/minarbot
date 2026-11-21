// Adapted from Bot HQ web/src/components/Menu.tsx (MIT, see NOTICE).
import { useEffect, useRef, type ReactNode } from 'react';

export interface MenuItem { label: string; onSelect: () => void; prefetch?: () => void; danger?: boolean; hint?: string; disabled?: boolean; icon?: ReactNode; separator?: boolean }

/** Small popover menu anchored by its parent (position: relative). Closes on outside click or Escape. */
export function Menu({ items, onClose, align = 'left', placement = 'below', children, label }: { items: MenuItem[]; onClose: () => void; align?: 'left' | 'right' | 'center'; placement?: 'below' | 'above'; children?: ReactNode; label?: string }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const down = (event: MouseEvent) => { if (ref.current && !ref.current.contains(event.target as Node)) onClose(); };
    const key = (event: KeyboardEvent) => { if (event.key === 'Escape') { event.stopPropagation(); onClose(); } };
    const timer = setTimeout(() => document.addEventListener('mousedown', down));
    document.addEventListener('keydown', key, true);
    ref.current?.querySelector<HTMLButtonElement>('[role="menuitem"]:not(:disabled)')?.focus();
    return () => { clearTimeout(timer); document.removeEventListener('mousedown', down); document.removeEventListener('keydown', key, true); };
  }, [onClose]);
  return (
    <div ref={ref} className={`menu is-${align} is-${placement}`} role="menu" aria-label={label}>
      {children}
      {items.map(item => (
        <button key={item.label} type="button" role="menuitem" disabled={item.disabled} onPointerEnter={item.prefetch} onFocus={item.prefetch} className={`menu__item${item.danger ? ' is-danger' : ''}${item.separator ? ' menu__item--separated' : ''}`} onClick={() => { onClose(); item.onSelect(); }}>
          {item.icon}<span>{item.label}</span>
          {item.hint && <span className="menu__hint">{item.hint}</span>}
        </button>
      ))}
    </div>
  );
}

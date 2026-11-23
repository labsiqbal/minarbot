import { messageRanges } from './messageFindRanges';
import { useLayoutEffect, useRef, useState, type RefObject } from 'react';
import { ChevronDown, ChevronUp, Search, X } from 'lucide-react';
import { IconButton } from './kit';

export function MessageFind({ query, onQuery, viewport, content, onClose }: {
  query: string; onQuery: (query: string) => void; viewport: RefObject<HTMLElement | null>; content: unknown; onClose: () => void;
}) {
  const ranges = useRef<Range[]>([]), index = useRef(0), previousQuery = useRef('');
  const [position, setPosition] = useState({ index: 0, total: 0 });
  function select(next: number, scroll = true) {
    const total = ranges.current.length;
    index.current = total ? (next + total) % total : 0;
    const range = ranges.current[index.current];
    CSS.highlights.set('message-find-current', new Highlight(...(range ? [range] : [])));
    setPosition(old => old.index === index.current && old.total === total ? old : { index: index.current, total });
    if (scroll && range && viewport.current) {
      const box = range.getBoundingClientRect(), view = viewport.current.getBoundingClientRect();
      const top = parseFloat(getComputedStyle(viewport.current).scrollPaddingTop);
      viewport.current.scrollTop += box.top - view.top - top - 16;
    }
  }
  useLayoutEffect(() => {
    if (!viewport.current) return;
    const empty = !ranges.current.length;
    ranges.current = messageRanges(viewport.current, query);
    const highlights = new Highlight();
    for (const range of ranges.current) highlights.add(range);
    CSS.highlights.set('message-find', highlights);
    const changed = previousQuery.current !== query;
    select(changed ? 0 : Math.min(index.current, ranges.current.length - 1), changed || empty);
    previousQuery.current = query;
    return () => { CSS.highlights.delete('message-find'); CSS.highlights.delete('message-find-current'); };
  }, [query, content, viewport]);
  return <form className="message-find bs-root" role="search" aria-label="Find in conversation" onSubmit={event => { event.preventDefault(); select(index.current + 1); }} onKeyDown={event => {
    if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); onClose(); }
    if (event.key === 'Enter' && event.shiftKey) { event.preventDefault(); select(index.current - 1); }
  }}>
    <Search size={15} aria-hidden /><input autoFocus aria-label="Find in conversation" type="text" role="searchbox" maxLength={200} value={query} onChange={event => onQuery(event.target.value)} />
    <span className="message-find__count" role="status">{position.total ? `${position.index + 1} of ${position.total}` : '0 of 0'}</span>
    <IconButton label="Previous match" disabled={!position.total} onClick={() => select(index.current - 1)}><ChevronUp size={15} aria-hidden /></IconButton>
    <IconButton label="Next match" disabled={!position.total} onClick={() => select(index.current + 1)}><ChevronDown size={15} aria-hidden /></IconButton>
    <IconButton label="Close search" onClick={onClose}><X size={15} aria-hidden /></IconButton>
  </form>;
}

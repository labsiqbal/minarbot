import { useId, useRef, useState } from 'react';
import { workflowEdges, workflowStepTypeLabel, type WorkflowGraph as Graph } from './workflows';
import { workflowLayout } from './workflow-editor-state';
import { Collapsible } from './ui/kit';
import './ui/workflow.css';

export function WorkflowGraph({ graph, selected, locked, onSelect, onMove, buttonRefs, labels, loops }: {
  graph: Graph; selected: string; locked: boolean; onSelect: (id: string) => void;
  buttonRefs: Map<string, HTMLButtonElement>;
  onMove?: (id: string, x: number, y: number) => void;
  labels?: ReadonlyMap<string, string>;
  loops?: ReadonlySet<string>;
}) {
  const viewport = useRef<HTMLDivElement>(null);
  const drag = useRef<{ id: string; pointer: number; startX: number; startY: number; lastX: number; lastY: number; scrollX: number; scrollY: number; x: number; y: number; moved: boolean } | null>(null);
  const [preview, setPreview] = useState<{ id: string; x: number; y: number } | null>(null), suppressClick = useRef(false);
  const marker = useId(), help = useId();
  const positions = workflowLayout(graph).map(point => preview?.id === point.id ? preview : point), byId = new Map(positions.map(item => [item.id, item]));
  function position(clientX: number, clientY: number) {
    const current = drag.current!;
    return { id: current.id, x: Math.max(64, Math.min(20000, Math.round(current.x + clientX - current.startX + (viewport.current?.scrollLeft ?? 0) - current.scrollX))), y: Math.max(64, Math.min(20000, Math.round(current.y + clientY - current.startY + (viewport.current?.scrollTop ?? 0) - current.scrollY))) };
  }
  const width = Math.max(360, ...positions.map(item => item.x + 244)), height = Math.max(200, ...positions.map(item => item.y + 144));
  const edges = graph.nodes.flatMap(node => workflowEdges(node).map((target, index) => ({ from: node.id, to: target, portY: node.kind === 'condition' ? index === 0 ? 20 : 60 : 40,
    label: node.kind === 'condition' ? index === 0 ? 'Matched' : 'Not matched' : node.kind === 'approval' ? 'Approved' : 'Continue' })));
  return <><p className="k-field__hint" id={help}>{onMove ? 'Drag nodes to position them. With a keyboard, focus a node and press Alt and an arrow key. Enter opens settings.' : 'Graph version when the run started. Status shows the last attempt for each step. Choose a node to see its details.'}</p><div ref={viewport} className="workflow-graph" role="region" aria-label="Workflow graph, scroll to see all steps" tabIndex={0} onScroll={() => {
    if (drag.current && !locked) { drag.current.moved = true; setPreview(position(drag.current.lastX, drag.current.lastY)); }
  }}>
    <div className="workflow-graph-content" style={{ width, height }}>
      <svg width={width} height={height} aria-hidden="true"><defs><marker id={marker} markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto"><path d="M0 0 L8 4 L0 8 Z" fill="var(--color-border-primary)" /></marker></defs>
        {edges.map((edge, index) => {
          const from = byId.get(edge.from), to = byId.get(edge.to); if (!from || !to) return null;
          const startX = from.x + 180, startY = from.y + edge.portY, endX = to.x, endY = to.y + edge.portY;
          const forward = endX > startX, laneY = Math.min(from.y, to.y) - 28 - index % 3 * 10;
          const path = forward ? `M${startX},${startY} C${startX + 50},${startY} ${endX - 50},${endY} ${endX},${endY}`
            : `M${startX},${startY} C${startX + 40},${startY} ${startX + 40},${laneY} ${startX},${laneY} L${endX},${laneY} C${endX - 40},${laneY} ${endX - 40},${endY} ${endX},${endY}`;
          return <g key={`${edge.from}:${index}`}><path d={path} fill="none" stroke="var(--color-border-primary)" strokeWidth="1.5" markerEnd={`url(#${marker})`} /><text x={(startX + endX) / 2} y={forward ? (startY + endY) / 2 - 8 : laneY - 5} textAnchor="middle">{edge.label}</text></g>;
        })}
      </svg>
      {graph.nodes.map(node => { const point = byId.get(node.id)!; return <button type="button" className={`workflow-graph-node${labels || loops?.has(node.id) ? ' workflow-run-node' : ''}`} data-movable={onMove ? '' : undefined} key={node.id}
        ref={element => { if (element) buttonRefs.set(node.id, element); else buttonRefs.delete(node.id); }}
        style={{ left: point.x, top: point.y }} disabled={locked} aria-current={selected === node.id ? 'step' : undefined}
        title={node.id} aria-describedby={help}
        onPointerDown={event => {
          if (locked || event.button !== 0 || !event.isPrimary) return;
          suppressClick.current = false;
          if (!onMove) return;
          event.currentTarget.setPointerCapture(event.pointerId);
          drag.current = { id: node.id, pointer: event.pointerId, startX: event.clientX, startY: event.clientY, lastX: event.clientX, lastY: event.clientY, scrollX: viewport.current?.scrollLeft ?? 0, scrollY: viewport.current?.scrollTop ?? 0, x: point.x, y: point.y, moved: false };
        }}
        onPointerMove={event => {
          const current = drag.current; if (!current || current.pointer !== event.pointerId || locked) return;
          current.lastX = event.clientX; current.lastY = event.clientY;
          if (Math.hypot(event.clientX - current.startX, event.clientY - current.startY) > 4) current.moved = true;
          if (current.moved) setPreview(position(event.clientX, event.clientY));
        }}
        onPointerUp={event => {
          const current = drag.current; if (!current || current.pointer !== event.pointerId) return;
          const target = position(event.clientX, event.clientY); suppressClick.current = current.moved;
          drag.current = null; setPreview(null);
          if (current.moved && !locked) onMove?.(node.id, target.x, target.y);
        }}
        onLostPointerCapture={() => { drag.current = null; setPreview(null); }}
        onPointerCancel={() => { suppressClick.current = !!drag.current; drag.current = null; setPreview(null); }}
        onKeyDown={event => {
          suppressClick.current = false;
          if (event.key === 'Escape') { suppressClick.current = !!drag.current; drag.current = null; setPreview(null); return; }
          if (!onMove || !event.altKey || !['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(event.key) || locked) return;
          event.preventDefault();
          onMove(node.id, point.x + (event.key === 'ArrowLeft' ? -20 : event.key === 'ArrowRight' ? 20 : 0), point.y + (event.key === 'ArrowUp' ? -20 : event.key === 'ArrowDown' ? 20 : 0));
        }}
        onClick={() => { if (suppressClick.current) suppressClick.current = false; else onSelect(node.id); }}><strong>{node.id}</strong><span>{workflowStepTypeLabel(node.kind)}{graph.entry === node.id ? ' · Start' : ''}</span>{labels ? <small>{labels.get(node.id) ?? 'Not run yet'}</small> : loops?.has(node.id) && <small>Unconditional loop</small>}</button>; })}
    </div>
  </div><div className="workflow-graph-links"><Collapsible title="Step connections"><ul>{edges.map((edge, index) => <li key={`${edge.from}:${index}`}>{edge.from}: {edge.label.toLowerCase()} to {edge.to || 'not selected yet'}{edge.to && !byId.has(edge.to) ? ' (step not found)' : ''}</li>)}</ul></Collapsible></div></>;
}

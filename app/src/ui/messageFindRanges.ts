import { matchOffsets } from './transcript';

export function messageRanges(root: HTMLElement, query: string) {
  const ranges: Range[] = [];
  for (const message of root.querySelectorAll('.human-bubble, .message-body .markdown, .routine-report__body .markdown')) {
    if (message.closest('[hidden]')) continue;
    const nodes: { node: Node; start: number; end: number }[] = [];
    let text = '';
    function visit(node: Node) {
      const element = node instanceof Element ? node : null;
      if (element?.matches('button, svg, .markdown-code__actions')) return;
      const block = element?.matches('p, h1, h2, h3, h4, h5, h6, li, pre, blockquote, td, th, br');
      if (block) text += '\n';
      if (node.nodeType === Node.TEXT_NODE) {
        const value = node.textContent ?? '';
        nodes.push({ node, start: text.length, end: text.length + value.length }); text += value;
      }
      for (const child of node.childNodes) visit(child);
      if (block) text += '\n';
    }
    visit(message);
    for (const match of matchOffsets(text, query)) {
      const first = nodes.find(item => item.end > match.start), last = nodes.find(item => item.end >= match.end);
      if (!first || !last) continue;
      const range = document.createRange();
      range.setStart(first.node, match.start - first.start); range.setEnd(last.node, match.end - last.start);
      ranges.push(range);
    }
  }
  return ranges;
}

import { parseRoute, routePath, type ResultTarget } from './navigation.ts';
import { navigateResult } from './hooks/useResultRoute.ts';
import type { AttachmentFile } from './attachments';
import { MessageCopyButton } from './ui/MessageCopyButton.ts';
import { createElement as h, memo, type MouseEvent, type ReactNode } from 'react';
import { marked, type Token, type Tokens } from 'marked';
import { decodeHTML } from 'entities';
import { splitMarkdownBlocks } from './ui/markdownStreamBlocks.ts';

import { markdownLink } from './markdown-link.ts';
export { markdownLink } from './markdown-link.ts';

export const Markdown = memo(function Markdown({ text, scope, origin, files = [], results = false }: { text: string; scope: string; origin: string; files?: AttachmentFile[]; results?: boolean }) {
  let count = 0, inLink = false;
  const mentioned = new Set<string>();
  const filePath = (file: AttachmentFile) => `${file.thread ? `/api/threads/${file.thread}` : scope}/files/${file.id}`;
  function link(href: string, label: ReactNode, key: string | number, file?: AttachmentFile, name?: string): ReactNode {
    if (file) mentioned.add(file.id);
    if (file?.deleted) return h('span', { key, className: 'message-muted', 'aria-label': `${file.name} (deleted)` }, label, ' (deleted)');
    const target = markdownLink(href, scope, origin) ?? (file ? { href, external: false } : null);
    if (!target) return h('span', { key }, label);
    if (!results && !files.length) return h('a', { key, href: target.href, ...(target.external ? { target: '_blank', rel: 'noopener noreferrer', title: target.href } : { download: true }) }, label);
    const result: ResultTarget = target.result ?? (target.external ? { view: 'url', path: new URL(target.href, origin).href, name: target.href } : { view: 'file', path: target.href.replace('/files/', '/panel/files/'), name: file?.name ?? name ?? (typeof label === 'string' ? label : 'File') });
    const route = typeof window === 'undefined' ? parseRoute(scope.replace('/api/agents/', '/agents/').replace('/api/channels/', '/rooms/')) : parseRoute(window.location.pathname + window.location.search);
    return h('a', { key, href: routePath({ ...route, result }), title: target.href, onClick: (event: MouseEvent<HTMLAnchorElement>) => {
      if (event.button || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
      event.preventDefault(); navigateResult(result);
    } }, label);
  }
  function prose(text: string, decode = true): ReactNode {
    text = decode ? decodeHTML(text) : text;
    if (inLink) return text;
    const names = files.map(file => file.name).filter(Boolean).sort((a, b) => b.length - a.length);
    if (!names.length) return text;
    const pattern = new RegExp(`(?<![\\p{L}\\p{N}_.-])(${names.map(name => name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|')})(?![\\p{L}\\p{N}_-]|\\.[\\p{L}\\p{N}])`, 'gu');
    return text.split(pattern).map((part, key) => {
      const file = files.find(file => file.name === part);
      return file ? link(filePath(file), part, key, file) : part;
    });
  }
  function render(tokens: Token[], depth = 0): ReactNode[] {
    if (depth > 40) throw new Error('Markdown depth');
    return tokens.map((token, key) => {
      if (++count > 6000) throw new Error('Markdown nodes');
      const child = () => render('tokens' in token ? token.tokens ?? [] : [], depth + 1);
      switch (token.type) {
        case 'space': case 'def': return null;
        case 'text': return token.tokens ? h('span', { key }, child()) : prose(token.text);
        case 'escape': return decodeHTML(token.text);
        case 'paragraph': return h('p', { key }, child());
        case 'heading': return h(`h${token.depth}`, { key, 'aria-level': Math.min(6, token.depth + 3) }, child());
        case 'strong': case 'em': case 'del': return h(token.type, { key }, child());
        case 'blockquote': return h('blockquote', { key }, child());
        case 'br': case 'hr': return h(token.type, { key });
        case 'codespan': return h('code', { key }, prose(token.text, false));
        case 'code': return h('div', { key, className: 'markdown-code' }, h('div', { className: 'markdown-code__actions bs-root' }, h('span', null, token.lang?.split(/\s/)[0] ?? ''), h(MessageCopyButton, { text: token.text, label: 'Copy code' })), h('pre', { tabIndex: 0, role: 'region', 'aria-label': 'Code block' }, h('code', null, token.text)));
        case 'checkbox': return h('span', { key, role: 'img', 'aria-label': token.checked ? 'Done' : 'Not done' }, token.checked ? '[x] ' : '[ ] ');
        case 'list': return h(token.ordered ? 'ol' : 'ul', { key, ...(token.ordered ? { start: token.start } : {}) }, token.items.map((item: Tokens.ListItem, index: number) => h('li', { key: index }, render(item.tokens, depth + 1))));
        case 'link': {
          const file = files.find(file => markdownLink(token.href, file.thread ? `/api/threads/${file.thread}` : scope, origin)?.href === filePath(file) || !/^https?:/i.test(token.href) && token.href.endsWith('/' + file.name));
          inLink = true; const label = child(); inLink = false;
          return link(file ? filePath(file) : token.href, label, key, file, token.text);
        }
        case 'image': return h('span', { key }, token.text ? `[Image: ${token.text}]` : '[Image]');
        case 'table': {
          count += token.header.length * (token.rows.length + 1) + token.rows.length;
          if (count > 6000) throw new Error('Markdown table nodes');
          const alignment = (cell: Tokens.TableCell) => ({ textAlign: cell.align ?? 'left' });
          return h('div', { key, className: 'markdown-table', tabIndex: 0, role: 'region', 'aria-label': 'Response table' }, h('table', null,
            h('thead', null, h('tr', null, token.header.map((cell: Tokens.TableCell, index: number) => h('th', { key: index, scope: 'col', style: alignment(cell) }, render(cell.tokens, depth + 1))))),
            h('tbody', null, token.rows.map((row: Tokens.TableCell[], index: number) => h('tr', { key: index }, row.map((cell, column) => h('td', { key: column, style: alignment(cell) }, render(cell.tokens, depth + 1))))))));
        }
        // Raw HTML remains visible source text, never DOM supplied by the model.
        default: return token.raw;
      }
    });
  }
  try {
    if (text.length > 64000) throw new Error('Markdown length');
    // ponytail: conservative pre-lexer table bound; a dedicated tokenizer can narrow false-positive plain-text fallback.
    const lines = text.split(/\r\n|\r|\n/), columns = lines.reduce((max, line) => Math.max(max, line.split('|').length), 1);
    if (columns * lines.length > 6000) throw new Error('Markdown table expansion');
    const content = render(marked.lexer(text, { gfm: true, breaks: false }));
    const remaining = files.filter(file => !mentioned.has(file.id));
    return h('div', { className: 'markdown' }, content, remaining.length ? h('p', null, remaining.flatMap((file, index) => [index ? ', ' : '', link(filePath(file), file.name, file.id, file)])) : null);
  } catch { return h('div', null, h('p', { className: 'message-text' }, text), files.length ? h('p', null, files.flatMap((file, index) => [index ? ', ' : '', link(filePath(file), file.name, file.id, file)])) : null); }
});

// Keep completed blocks mounted and memoized while only the trailing block changes.
// Document-wide syntax falls back to one renderer to preserve Markdown semantics.
export function StreamingMarkdown(props: { text: string; scope: string; origin: string; files?: AttachmentFile[] }) {
  const blocks = !props.files?.length && props.text.length <= 64000 ? splitMarkdownBlocks(props.text) ?? [props.text] : [props.text];
  return h('div', { className: 'stream-markdown' }, blocks.map((text, index) =>
    h(Markdown, { ...props, results: true, text, key: index })));
}

import { marked, type Tokens } from 'marked';
import { markdownLink } from './markdown-link';

const escape = (value: string) => value.replaceAll('&', '&amp;').replaceAll('<', '&lt;')
  .replaceAll('>', '&gt;').replaceAll('"', '&quot;');

export function renderWorkflowMarkdown(text: string, scope: string) {
  const lines = text.split(/\r\n|\r|\n/);
  const columns = lines.reduce((max, line) => Math.max(max, line.split('|').length), 1);
  if (text.length > 64000 || columns * lines.length > 6000) return escape(text);
  const renderer = Object.assign(new marked.Renderer(), {
    html: ({ text }: Tokens.HTML | Tokens.Tag) => escape(text),
    image: ({ text }: Tokens.Image) => escape(text ? `[Image: ${text}]` : '[Image]'),
    link(this: { parser: { parseInline: (tokens: Tokens.Link['tokens']) => string } }, token: Tokens.Link) {
      const label = this.parser.parseInline(token.tokens);
      const target = markdownLink(token.href, scope, location.origin);
      if (!target) return label;
      const attributes = target.external ? ' target="_blank" rel="noopener noreferrer"' : target.result ? '' : ' download';
      return `<a href="${escape(target.href)}"${attributes}>${label}</a>`;
    },
  });
  try { return marked.parse(text, { async: false, renderer }); }
  catch { return escape(text); }
}

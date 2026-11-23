// Adapted from T3 (MIT); source hashes and notices: app/t3/provenance.json.
import { marked, type Token, type Tokens } from 'marked';
import { decodeHTML } from 'entities';

export type TimelineMinimapItem = { id: string; rowIndex: number; userText: string | null; assistantText: string | null };

// Reuse Minarbot's Markdown lexer/entity decoder; preview never creates model-supplied DOM.
function tokenText(tokens: Token[], depth = 0): string {
  if (depth > 40) return '';
  return tokens.map(token => {
    if (token.type === 'space' || token.type === 'br' || token.type === 'hr') return ' ';
    if (token.type === 'def') return '';
    if (token.type === 'list') return token.items.map((item: Tokens.ListItem) => tokenText(item.tokens, depth + 1)).join(' ');
    if (token.type === 'table') return [token.header, ...token.rows].map(row => row.map((cell: Tokens.TableCell) => tokenText(cell.tokens, depth + 1)).join(' ')).join(' ');
    const text = 'tokens' in token && token.tokens ? tokenText(token.tokens, depth + 1) : 'text' in token ? token.text : token.raw;
    return ['paragraph', 'heading', 'blockquote', 'code'].includes(token.type) ? ` ${text} ` : text;
  }).join('');
}

export function resolveTimelineMinimapPreview(item: TimelineMinimapItem | null): TimelineMinimapItem | null {
  const compact = (text: string | null) => text?.replace(/\s+/g, ' ').trim() || null;
  return item && { ...item, userText: compact(item.userText), assistantText: compact(item.assistantText === null ? null : decodeHTML(tokenText(marked.lexer(item.assistantText.slice(0, 1500))))) };
}

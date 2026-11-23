import test from 'node:test';
import assert from 'node:assert/strict';
import { marked } from 'marked';
import { splitMarkdownBlocks } from './src/ui/markdownStreamBlocks.ts';

test('stream blocks preserve full-document Markdown semantics at every prefix', () => {
  const examples = [
    'First **paragraph**.\n\nSecond paragraph.\n\n# Heading\n\nEnd',
    '- First\n\n- Second\n\n  Continued\n\nAfter list',
    '> Quote\n> continued\n\nNext',
    '```ts\nconst value = 1;\n\nvalue;\n```\n\nAfter fence',
    '~~~text\ninside\n\n~~~\n\nAfter',
    '| A | B |\n| - | - |\n| 1 | 2 |\n\nAfter table',
    'Title\n=====\n\nAfter',
    '- ```\n  code\n\n  ```\n\nAfter',
  ];
  for (const text of examples) for (let n = 0; n <= text.length; n++) {
    const prefix = text.slice(0, n), blocks = splitMarkdownBlocks(prefix);
    if (blocks) {
      assert.equal(blocks.join(''), prefix);
      assert.equal(blocks.map(block => marked.parse(block)).join(''), marked.parse(prefix), JSON.stringify(prefix));
    }
  }
});
test('cross-block syntax keeps one renderer; completed block prefixes stay stable', () => {
  for (const text of ['[link][id]\n\n[id]: https://example.com', 'a\r\n\r\nb', '<div>\na\n\nb\n</div>', 'a[^1]\n\n[^1]: note', '$$\nx\n\n$$', ':::note\nx']) assert.equal(splitMarkdownBlocks(text), null);
  const complete = 'First block.\n\n';
  for (const suffix of ['Second', 'Second grows', 'Second grows.\n\nThird']) assert.equal(splitMarkdownBlocks(complete + suffix)?.[0], complete);
});

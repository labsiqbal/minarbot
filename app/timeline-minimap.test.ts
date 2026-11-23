// Adapted from T3 minimap tests (MIT); see app/t3/provenance.json.
import test from 'node:test';
import assert from 'node:assert/strict';
import { resolveTimelineMinimapPreview } from './src/ui/timelineMinimapItems.ts';

test('preview compacts user text and renders Markdown text without link destinations', () => {
  const preview = (assistantText: string | null, userText = '  A\n question  ') => resolveTimelineMinimapPreview({ id: 'one', rowIndex: 0, userText, assistantText });
  assert.equal(resolveTimelineMinimapPreview(null), null);
  assert.deepEqual(preview('## Answer\n\n**Ready** and [Docs](https://example.test/private).\n\n- One\n- Two\n\n`code` &amp; text'), { id: 'one', rowIndex: 0, userText: 'A question', assistantText: 'Answer Ready and Docs. One Two code & text' });
  assert.equal(preview('  ')?.assistantText, null);
  assert.equal(preview('x'.repeat(2000))?.assistantText?.length, 1500);
  assert.equal(preview('| Name | Value |\n| --- | --- |\n| A | B |')?.assistantText, 'Name Value A B');
  assert.equal(preview('<img src="https://tracker.test">')?.assistantText, '<img src="https://tracker.test">', 'Match Minarbot Markdown: raw HTML stays inert text');
});

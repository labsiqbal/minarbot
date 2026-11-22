import assert from 'node:assert/strict';

export async function graphLayout(page, receipt, label) {
  const graph = page.getByRole('region', { name: 'Workflow graph, scroll to see all steps', exact: true });
  await graph.waitFor({ state: 'visible' });
  await graph.scrollIntoViewIfNeeded();
  const layout = await graph.evaluate(element => {
    const content = element.querySelector('.workflow-graph-content');
    const box = content.getBoundingClientRect();
    return {
      content: { width: box.width, height: box.height, position: getComputedStyle(content).position },
      background: getComputedStyle(element).backgroundColor,
      nodes: [...element.querySelectorAll('.workflow-graph-node')].map(node => {
        const rect = node.getBoundingClientRect(), style = getComputedStyle(node);
        return { id: node.title, x: rect.x - box.x, y: rect.y - box.y, width: rect.width, height: rect.height,
          position: style.position, border: parseFloat(style.borderTopWidth), borderStyle: style.borderTopStyle,
          left: parseFloat(node.style.left), top: parseFloat(node.style.top) };
      }),
      edges: [...element.querySelectorAll('svg g text')].map(text => ({ text: text.textContent, fill: getComputedStyle(text).fill, color: getComputedStyle(text).color })),
    };
  });
  receipt.graphLayouts.push({ label, ...layout });
  assert.equal(layout.content.position, 'relative', 'Graph must anchor positioned cards');
  assert.equal(layout.nodes.length, 2, 'Connected fixture must render both steps');
  for (const node of layout.nodes) {
    assert.equal(node.position, 'absolute', `${node.id}: graph stylesheet missing`);
    assert(node.width > 100 && node.height > 40 && node.border > 0 && node.borderStyle !== 'none', `${node.id}: not a sized card`);
    assert(Math.abs(node.x - node.left) < 1 && Math.abs(node.y - node.top) < 1, `${node.id}: card ignores saved position`);
    assert(node.y >= 0 && node.y + node.height < layout.content.height, `${node.id}: card stacked below graph`);
  }
  const [first, second] = layout.nodes;
  assert(first.x + first.width <= second.x || second.x + second.width <= first.x || first.y + first.height <= second.y || second.y + second.height <= first.y, 'Graph cards overlap');
  assert.equal(layout.edges.length, 1, 'Fixture edge label missing');
  for (const edge of layout.edges) {
    assert(edge.fill !== layout.background && !['none', 'transparent', 'rgba(0, 0, 0, 0)'].includes(edge.fill), 'Edge label paint matches graph background');
    assert.notEqual(edge.color, layout.background, 'Edge label color matches graph background');
  }
  assert.deepEqual(receipt.cspViolations, [], 'Graph emitted CSP violations');
}

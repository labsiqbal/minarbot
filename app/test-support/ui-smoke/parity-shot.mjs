export const fixtureClock = Date.UTC(2026, 10, 23, 7);

// Render full tiles in grayscale AA. Headless LCD rasterization depends on layer
// history, even when the native dialog and its React counterpart have identical geometry.
export const parityChromiumArgs = ['--disable-lcd-text', '--disable-partial-raster'];

export function fixDate(page, time = fixtureClock, { tick = false } = {}) {
  return page.addInitScript(({ time, tick }) => {
    const now = () => time + (tick ? Math.floor(performance.now()) : 0);
    globalThis.Date = new Proxy(Date, {
      apply: target => new target(now()).toString(),
      construct: (target, args, constructor) => Reflect.construct(target, args.length ? args : [now()], constructor),
      get: (target, key, receiver) => key === 'now' ? now : Reflect.get(target, key, receiver),
    });
  }, { time, tick });
}

export async function parityShot(page, path, region = page) {
  await page.evaluate(() => {
    const normalize = value => value
      .replace(/\b[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\b/gi, '00000000-0000-4000-8000-000000000000')
      .replace(/127\.0\.0\.1:\d{1,5}/g, '127.0.0.1:00000')
      .replace(/\/home\/[^\s/]+\/\.t3\/worktrees\/[^\s/]+/g, '/tmp/minarbot-fixture')
      .replace(/\b\d{1,2}:\d{2}:[0-5]\d (?:AM|PM)\b/g, '12:00:00 PM');
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    const nodes = [];
    while (walker.nextNode()) nodes.push(walker.currentNode);
    for (const node of nodes) {
      const value = normalize(node.nodeValue ?? '');
      if (value !== node.nodeValue) node.nodeValue = value;
    }
    for (const input of document.querySelectorAll('input, textarea')) {
      const value = normalize(input.value);
      if (value !== input.value) input.value = value;
    }
  });
  await page.evaluate(async () => {
    await document.fonts.ready;
    await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
  });
  return region.screenshot({ path, animations: 'disabled', caret: 'hide', scale: 'css' });
}

// Substitute only values supplied by the disposable fixture during capture.
export async function withFixtureText(page, replacements, capture) {
  const restore = await page.evaluateHandle(replacements => {
    const changes = [];
    const apply = (node, field) => {
      const before = node[field];
      let after = before;
      for (const [from, to] of replacements) after = after.replaceAll(from, to);
      if (before === after) return;
      node[field] = after;
      changes.push([node, field, before, after]);
    };
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT, {
      acceptNode: node => node.parentElement?.closest('script, style') ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT,
    });
    while (walker.nextNode()) apply(walker.currentNode, 'data');
    for (const input of document.querySelectorAll('input[readonly], textarea[readonly]')) apply(input, 'value');
    return () => {
      for (const [node, field, before, after] of changes) {
        if (node[field] === after) node[field] = before;
      }
    };
  }, replacements);
  try {
    return await capture();
  } finally {
    await restore.evaluate(restore => restore());
    await restore.dispose();
  }
}

export async function withFixtureId(page, id, action) {
  await page.evaluate(id => {
    crypto.randomUUID = () => {
      delete crypto.randomUUID;
      return id;
    };
  }, id);
  try {
    await action();
  } finally {
    await page.evaluate(() => { delete crypto.randomUUID; });
  }
}

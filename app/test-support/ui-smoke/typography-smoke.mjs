import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { createRequire } from 'node:module';
const sharp = createRequire(new URL('../../package.json', import.meta.url))('sharp');

export async function typographySmoke({ page, check, origin, agentChat, sidebarTab, send, root, baseline = false }) {
  const phase = baseline ? 'before' : 'after';
  const output = join(root, 'app/test-support/ui-smoke/artifacts/typography-polish');
  await mkdir(output, { recursive: true });
  const evidence = { phase, origin, cases: [], fonts: [] };
  const post = async (path, data) => {
    const response = await page.request.post(origin + path, { headers: { Origin: origin }, data });
    assert(response.ok(), await response.text()); return response.json();
  };
  const lead = await post('/api/agents', {runtime: 'api',  id: randomUUID(), name: 'Typography Lead', model: 'smoke-primary', instructions: 'Reply briefly.' });
  const helper = await post('/api/agents', {runtime: 'api',  id: randomUUID(), name: 'Typography Helper', model: 'smoke-primary', instructions: 'Reply briefly.' });
  await post('/api/projects', { id: randomUUID(), name: 'Typography', members: [{ agent: lead.id, write: false }, { agent: helper.id, write: false }] });
  const client = await page.context().newCDPSession(page);
  await client.send('DOM.enable'); await client.send('CSS.enable');
  async function renderedFont(selector) {
    const { root } = await client.send('DOM.getDocument');
    const { nodeId } = await client.send('DOM.querySelector', { nodeId: root.nodeId, selector });
    assert(nodeId, `Missing font probe ${selector}`);
    return (await client.send('CSS.getPlatformFontsForNode', { nodeId })).fonts;
  }
  try {
    for (const kind of ['direct', 'room']) await check(`TY1 ${phase} ${kind} typography matrix`, async () => {
      await page.reload(); await agentChat();
      await page.getByRole('navigation', { name: 'Choose agent', exact: true }).getByRole('button', { name: /^Typography Lead / }).click();
      let name = 'Typography Lead';
      if (kind === 'room') {
        await sidebarTab('Projects');
        await page.getByRole('button', { name: /^# general in Typography/ }).click();
        name = 'Typography / general';
      }
      await send(name, 'Review the release plan and list the next step.');
      const scope = page.getByRole('main', { name: `Conversation ${name}`, exact: true });
      await scope.locator('.message-body .markdown p').first().waitFor();
      await page.evaluate(() => document.fonts.ready);
      const font = await page.evaluate(() => ({ body: getComputedStyle(document.body).fontFamily, loaded: document.fonts.check('14px "Inter Variable"') }));
      font.rendered = await renderedFont('.message-body .markdown p');
      font.kind = kind; evidence.fonts.push(font); console.log('TY1 font:', JSON.stringify(font));
      if (!baseline) {
        assert.match(font.body, /^"?Inter Variable/); assert(font.loaded);
        assert(font.rendered.some(font => /^Inter(?: Variable)?$/.test(font.familyName) && font.isCustomFont && font.glyphCount > 0));
        assert.equal(await scope.locator('.composer__foot').count(), 0);
        assert(await scope.locator('.turn-controls button').evaluateAll(buttons => buttons.every(button => !button.textContent.trim())), 'Reply actions must stay icon-only');
      }
      const captures = new Map();
      for (const width of [1440, 1024]) {
        await page.setViewportSize({ width, height: width === 1440 ? 900 : 768 });
        await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
        const sheet = page.getByRole('dialog', { name: 'Conversation panel', exact: true });
        if (await sheet.isVisible()) await sheet.getByRole('button', { name: 'Close details', exact: true }).click();
        for (const size of ['default', 'compact', 'large', 'larger']) for (const palette of ['minarbot', 'github', 'catppuccin', 'everforest', 'solarized', 'midnight']) for (const theme of ['dark', 'light']) {
          await page.evaluate(values => Object.assign(document.documentElement.dataset, values), { textSize: size, colorTheme: palette, theme });
          const layout = await scope.evaluate(el => {
            const css = selector => getComputedStyle(el.querySelector(selector));
            const human = css('.human-bubble'), reply = css('.message-body .markdown p'), summary = el.querySelector('.turn-activity .activity-label');
            return { overflow: document.documentElement.scrollWidth > innerWidth + 1 || el.scrollWidth > el.clientWidth + 1,
              human: [human.fontSize, human.lineHeight], reply: [reply.fontSize, reply.lineHeight],
              tracking: getComputedStyle(document.body).letterSpacing, innerGap: css('.turn-block').gap, outerGap: css('.transcript__col').gap,
              border: el.querySelector('.turn-activity') ? css('.turn-activity').borderBottomWidth : null,
              numeric: summary ? getComputedStyle(summary).fontVariantNumeric : css('.room-agent__status').fontVariantNumeric };
          });
          evidence.cases.push({ kind, width, size, palette, theme, ...layout });
          assert(!layout.overflow, `${kind}/${width}/${size}/${palette}/${theme}: overflow`);
          if (!baseline) {
            assert.deepEqual(layout.reply, layout.human); assert(['normal', '0px'].includes(layout.tracking));
            assert.equal(parseFloat(layout.innerGap), parseFloat(layout.outerGap), 'T3 uses 16px row spacing within and between turns');
            if (layout.border !== null) assert.equal(layout.border, '0px');
            assert.equal(layout.numeric, 'tabular-nums');
          }
          if (!baseline) {
            const key = `${kind}-${width}-${size}`;
            const tiles = captures.get(key) ?? [];
            const tile = await sharp(await page.screenshot({ animations: 'disabled' })).resize(576, 432, { fit: 'contain', background: 'white' }).composite([{ input: Buffer.from(`<svg width="576" height="24"><rect width="576" height="24" fill="white"/><text x="8" y="17" font-size="14" fill="black">${kind} ${width}px ${size} ${palette} ${theme}</text></svg>`), top: 0, left: 0 }]).png().toBuffer();
            tiles.push({ input: tile, left: (tiles.length % 3) * 576, top: Math.floor(tiles.length / 3) * 432 });
            captures.set(key, tiles);
          }
          if (width === 1440 && ['default', 'compact'].includes(size) && palette === 'minarbot' && theme === 'dark') {
            await page.mouse.move(0, 0); await scope.locator('.transcript').evaluate(el => { el.scrollTop = el.scrollHeight; });
            await page.screenshot({ path: join(output, `${phase}-${kind}-${size}.png`), animations: 'disabled' });
          }
        }
      }
      if (!baseline) {
        assert.equal(await scope.getByRole('button', { name: 'View task', exact: true }).count(), 0);
        const message = scope.getByRole('textbox', { name: kind === 'room' ? 'Message #general' : `Message ${name}`, exact: true });
        await message.fill('Keep the final reply concise and ready to share.'); await message.press('Enter');
        await scope.locator('.turn-block').nth(1).locator('.message-body .markdown p').first().waitFor();
        assert.equal(await scope.locator('.turn-block > .human-message').count(), 2);
        const rhythm = await scope.locator('.transcript__col').evaluate(el => {
          const turns = [...el.querySelectorAll(':scope > .turn-block')];
          const first = turns[0].getBoundingClientRect(), second = turns[1].getBoundingClientRect();
          return { between: second.top - first.bottom, within: parseFloat(getComputedStyle(turns[0]).gap) };
        });
        assert(Math.abs(rhythm.between - rhythm.within) <= 1, JSON.stringify(rhythm));
      }
      for (const [key, tiles] of captures) await sharp({ create: { width: 1728, height: 1728, channels: 3, background: 'white' } }).composite(tiles).png().toFile(join(output, `matrix-${key}.png`));
    });
    await check(`TY1 ${phase} Linux monospace`, async () => {
      await page.evaluate(() => { const code = document.createElement('code'); code.id = 'ty1-mono'; code.textContent = 'iiii 1111 WWWW'; code.style.fontFamily = 'var(--font-mono-stack)'; document.querySelector('.message-body').append(code); });
      await page.locator('#ty1-mono').scrollIntoViewIfNeeded();
      const fonts = await renderedFont('#ty1-mono'); evidence.mono = fonts;
      assert(fonts.some(font => /mono/i.test(font.familyName) && font.glyphCount > 0), JSON.stringify(fonts));
      await page.evaluate(() => document.getElementById('ty1-mono').remove());
    });
    await check(`TY1 ${phase} hub font and text-size matrix`, async () => {
      const hub = join(root, 'app/dist/_leptos');
      const documentRoute = async route => {
        const html = await readFile(join(hub, 'hub.html'), 'utf8');
        await route.fulfill({ contentType: 'text/html', body: html.replace('<head>', '<head><meta name="csp-nonce" content="ty1"><script id="hub-config" type="application/json">{"accountsHost":"127.0.0.1","domain":"example.test","developmentIdentity":false}</script>') });
      };
      const sessionRoute = route => route.fulfill({ json: { accountId: randomUUID(), name: 'Typography fixture', csrf: 'ty1', expiresAt: Date.now() + 60000 } });
      const projectsRoute = route => route.fulfill({ json: [] });
      await page.route('**/ty1-hub', documentRoute); await page.route('**/api/session', sessionRoute); await page.route('**/api/projects', projectsRoute);
      try {
        await page.goto(origin + '/ty1-hub');
        await page.getByRole('heading', { name: 'Projects', exact: true }).waitFor();
        await page.evaluate(() => document.fonts.ready);
        const font = await page.evaluate(() => ({ body: getComputedStyle(document.body).fontFamily, loaded: document.fonts.check('14px "Inter Variable"') }));
        font.rendered = await renderedFont('.hub-main h1'); font.kind = 'hub'; evidence.fonts.push(font);
        console.log('TY1 font:', JSON.stringify(font));
        assert.match(font.body, /^"?Inter Variable/); assert(font.loaded);
        assert(font.rendered.some(font => /^Inter(?: Variable)?$/.test(font.familyName) && font.isCustomFont && font.glyphCount > 0));
        evidence.hub = [];
        for (const [size, expected] of [['compact', 14], ['default', 16], ['large', 18], ['larger', 20]]) {
          await page.evaluate(size => { document.documentElement.dataset.textSize = size; }, size);
          const actual = await page.evaluate(() => parseFloat(getComputedStyle(document.documentElement).fontSize));
          evidence.hub.push({ size, actual }); if (!baseline) assert.equal(actual, expected);
        }
      } finally {
        await page.unroute('**/ty1-hub', documentRoute); await page.unroute('**/api/session', sessionRoute); await page.unroute('**/api/projects', projectsRoute);
        await page.goto(origin);
      }
    });
  } finally {
    await client.detach();
    await writeFile(join(output, `${phase}.json`), JSON.stringify(evidence, null, 2) + '\n');
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.evaluate(() => Object.assign(document.documentElement.dataset, { textSize: 'default', colorTheme: 'minarbot', theme: 'dark' }));
  }
}

import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir } from 'node:fs/promises';
import { join } from 'node:path';

export async function timelineMinimapSmoke({ check, page, origin, root, agentChat }) {
  const post = async (path, data) => {
    const response = await page.request.post(origin + path, { headers: { Origin: origin }, data });
    assert(response.ok(), await response.text()); return response.json();
  };
  const agent = await post('/api/agents', {runtime: 'api',  id: randomUUID(), name: 'MM1 Minimap', instructions: 'Synthetic minimap fixture.', model: 'smoke-primary' });
  const project = await post('/api/projects', { id: randomUUID(), name: 'MM1 Room', members: [{ agent: agent.id, write: false }] });
  const directPath = `/api/agents/${agent.id}/turns`, roomPath = `/api/channels/${project.sections[0].id}/turns`;
  const makeTurns = count => Array.from({ length: count }, (_, index) => ({ id: randomUUID(), seq: index + 101, text: `Minimap question ${index + 1}`, reply: `**Answer ${index + 1}** with [reference](https://example.test/hidden-target).\n\n${'Synthetic transcript content. '.repeat(14)}`, agent: agent.id, status: 'done', error: null, paused: 0, revision: 1, no_update: 0, created: '2026-11-15T00:00:00Z' }));
  let turns = makeTurns(1);
  const older = makeTurns(6);
  const route = async route => {
    const url = new URL(route.request().url());
    if (route.request().method() !== 'GET' || ![directPath, roomPath].includes(url.pathname)) return route.continue();
    const rows = url.searchParams.has('before') ? older : turns;
    return route.fulfill({ contentType: 'application/json', body: JSON.stringify(rows) });
  };
  const pattern = '**/api/**/turns*';
  await page.route(pattern, route);
  const open = async room => {
    await page.reload();
    await page.getByRole('tab', { name: room ? /^Projects/ : /^Agents/ }).click();
    await page.getByRole('button', { name: room ? /^# general in MM1 Room/ : /^MM1 Minimap / }).click();
    await page.locator('.turn-block').first().waitFor();
    const hide = page.getByRole('button', { name: 'Close details', exact: true });
    if (await hide.isVisible()) await hide.click();
  };
  const rail = page.locator('.timeline-minimap__hit'), ticks = page.locator('[data-minimap-strip]');
  const waitCount = async count => page.waitForFunction(count => document.querySelectorAll('[data-minimap-strip]').length === count, count);
  const point = async index => {
    const box = await rail.boundingBox(); assert(box);
    return { x: box.x + 3, y: box.y + box.height * index / (await ticks.count() - 1) };
  };
  const aligned = async id => page.waitForFunction(id => {
    const row = document.getElementById(`turn-${id}`), viewport = document.querySelector('.transcript');
    const top = row.getBoundingClientRect().top - viewport.getBoundingClientRect().top;
    return Math.abs(top - parseFloat(getComputedStyle(viewport).scrollPaddingTop)) <= 1 || viewport.scrollTop === 0 && top >= 0 || viewport.scrollHeight - viewport.scrollTop - viewport.clientHeight <= 1 && top >= 0 && top < viewport.clientHeight;
  }, id);
  const evidence = join(root, 'app/test-support/ui-smoke/artifacts/timeline-minimap');
  await mkdir(evidence, { recursive: true });
  try {
    await check('MM1 few turns hidden; human ticks, hover previews, click and keyboard jumps', async () => {
      await page.setViewportSize({ width: 1440, height: 900 }); await open(false);
      assert.equal(await ticks.count(), 0);
      turns = makeTurns(2).map(turn => ({ ...turn, reply: 'Finished.', activity: { calls: [], tools: [{ id: 'shell', name: 'run_shell', state: 'completed' }] } }));
      await open(false);
      await page.getByRole('button', { name: 'Worked', exact: true }).first().click();
      await page.getByRole('list', { name: 'Turn activity' }).waitFor();
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      assert.equal(await ticks.count(), 0, 'History that fits on screen needs no minimap');
      turns = makeTurns(12); await open(false); await waitCount(12);
      await page.waitForFunction(() => document.querySelector('.timeline-minimap')?.dataset.persistentGutter === 'true');
      const next = page.getByRole('button', { name: 'Next turn', exact: true });
      assert(await next.isDisabled());
      assert.equal(await next.evaluate(button => getComputedStyle(button).opacity), '0');
      assert.equal(await page.locator('.human-message').count(), 12);
      for (const theme of ['dark', 'light']) {
        await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
        const position = await point(3); await page.mouse.move(position.x, position.y);
        const preview = page.locator('.timeline-minimap__preview'); await preview.waitFor();
        assert.match(await preview.textContent(), /Minimap question 4Answer 4 with reference\./);
        assert(!(await preview.textContent()).includes('hidden-target'));
        await page.screenshot({ path: join(evidence, `${theme}-hover.png`), animations: 'disabled' });
      }
      const position = await point(3); await page.mouse.click(position.x, position.y); await aligned(turns[3].id);
      await rail.focus(); await rail.press('Home'); await rail.press('Enter'); await aligned(turns[0].id);
      await rail.press('ArrowDown'); await rail.press('Space'); await aligned(turns[1].id);
      await rail.press('End'); await rail.press('ArrowUp'); await rail.press('Enter'); await aligned(turns[10].id);
      await rail.press('Escape'); assert.equal(await page.locator('.timeline-minimap__preview').count(), 0);
      await page.getByRole('button', { name: 'Previous turn', exact: true }).click(); await aligned(turns[9].id);
      await page.getByRole('button', { name: 'Next turn', exact: true }).click(); await aligned(turns[10].id);
    });
    await check('MM1 visible-range highlight, wheel forwarding and streaming preserve free scroll', async () => {
      await rail.focus(); await rail.press('Home'); await rail.press('Enter'); await aligned(turns[0].id);
      await page.waitForFunction(() => {
        const viewport = document.querySelector('.transcript').getBoundingClientRect();
        return [...document.querySelectorAll('[data-minimap-strip]')].every(tick => {
          const row = document.getElementById(`turn-${tick.dataset.turn}`).querySelector('.human-message').getBoundingClientRect();
          return tick.dataset.inView === String(row.top < viewport.bottom && row.bottom > viewport.top);
        });
      });
      const inView = await page.locator('[data-minimap-strip][data-in-view=true]').count(); assert(inView > 0 && inView < 12);
      const before = await page.locator('.transcript').evaluate(el => el.scrollTop);
      await rail.dispatchEvent('wheel', { deltaY: 120, deltaMode: 0 });
      await page.waitForFunction(before => document.querySelector('.transcript').scrollTop === before + 120, before);
      const pixelTop = await page.locator('.transcript').evaluate(el => el.scrollTop);
      await rail.dispatchEvent('wheel', { deltaY: 3, deltaMode: 1 });
      await page.waitForFunction(before => document.querySelector('.transcript').scrollTop === before + 48, pixelTop);
      await rail.dispatchEvent('wheel', { deltaY: 120, ctrlKey: true });
      assert.equal(await page.locator('.transcript').evaluate(el => el.scrollTop), pixelTop + 48);
      await rail.evaluate(el => {
        for (let i = 0; i < 2; i++) {
          const event = new WheelEvent('wheel', { deltaY: 100, cancelable: true, bubbles: true });
          Object.defineProperty(event, 'wheelDeltaY', { value: -120 }); el.dispatchEvent(event);
        }
      });
      await page.waitForFunction(before => document.querySelector('.transcript').scrollTop === before + 200, pixelTop + 48);
      const pageStart = await page.locator('.transcript').evaluate(el => el.scrollTop);
      const height = await page.locator('.transcript').evaluate(el => el.clientHeight);
      await rail.dispatchEvent('wheel', { deltaY: 1, deltaMode: 2 });
      await page.waitForFunction(({ top, height }) => document.querySelector('.transcript').scrollTop === top + height, { top: pageStart, height });
      await rail.evaluate(el => {
        const event = new WheelEvent('wheel', { deltaY: 100, cancelable: true, bubbles: true });
        Object.defineProperty(event, 'wheelDeltaY', { value: -120 }); el.dispatchEvent(event);
        document.querySelector('.transcript').scrollTop = 1000;
        window.dispatchEvent(new PointerEvent('pointerdown'));
      });
      await page.waitForTimeout(200);
      assert.equal(await page.locator('.transcript').evaluate(el => el.scrollTop), 1000, 'Pointer input must cancel mouse-wheel glide');
      await page.locator('.timeline-minimap__rail').dispatchEvent('mousemove', { buttons: 1 });
      assert.equal(await page.locator('.timeline-minimap__rail').evaluate(el => getComputedStyle(el).pointerEvents), 'none', 'Text selection must pass through rail');
      await page.evaluate(() => window.dispatchEvent(new MouseEvent('mouseup', { buttons: 0 })));
      const top = await page.locator('.transcript').evaluate(el => el.scrollTop);
      turns = turns.map((turn, index) => index === 11 ? { ...turn, reply: null, status: 'running', partial: { text: 'Growing preview text', status: 'pending', seq: 1 } } : turn);
      await page.getByLabel('Streaming answer', { exact: true }).waitFor({ state: 'attached' });
      assert.equal(await page.locator('.transcript').evaluate(el => el.scrollTop), top);
      const position = await point(11); await page.mouse.move(position.x, position.y);
      await page.locator('.timeline-minimap__preview').getByText('Growing preview text', { exact: true }).waitFor();
      turns = turns.map(turn => ({ ...turn, status: 'done', reply: turn.reply ?? 'Final preview text', partial: null }));
      await page.getByRole('button', { name: 'Jump to latest', exact: true }).click();
      await page.waitForFunction(() => { const el = document.querySelector('.transcript'); return el.scrollHeight - el.scrollTop - el.clientHeight <= 1; });
      await page.setViewportSize({ width: 720, height: 768 }); assert(!(await rail.isVisible()));
      await page.setViewportSize({ width: 1440, height: 900 }); await rail.waitFor();
    });
    await check('MM1 paged older history and room human-turn grouping', async () => {
      turns = makeTurns(100); await open(false); await waitCount(100);
      await page.getByRole('button', { name: 'Older messages', exact: true }).click(); await waitCount(6);
      await rail.focus(); await rail.press('Home'); await rail.press('Enter'); await aligned(older[0].id);
      await page.getByRole('button', { name: 'Newer messages', exact: true }).click(); await waitCount(100);
      older[0].text = 'Older archive needle';
      await page.keyboard.press('Control+f');
      const find = page.getByRole('search', { name: 'Find in conversation', exact: true }); await find.waitFor();
      await find.getByRole('searchbox').fill('archive needle');
      await find.getByRole('status').getByText('1 of 1', { exact: true }).waitFor();
      assert.equal(await page.locator('.turn-block').count(), 106);
      assert.equal(await page.evaluate(() => [...CSS.highlights.get('message-find-current')][0].toString()), 'archive needle');
      const matchBox = await page.evaluate(() => [...CSS.highlights.get('message-find-current')][0].getBoundingClientRect().top);
      assert(matchBox >= 116, 'Find match sits under floating chrome');
      await find.getByRole('button', { name: 'Next match', exact: true }).click();
      await find.getByRole('button', { name: 'Previous match', exact: true }).click();
      await find.getByRole('searchbox').fill('answer 1');
      await page.waitForFunction(() => CSS.highlights.get('message-find').size > 1);
      await find.getByRole('searchbox').fill('not-present-in-history');
      await find.getByRole('status').getByText('0 of 0', { exact: true }).waitFor();
      await find.getByRole('searchbox').press('Escape'); await find.waitFor({ state: 'hidden' });
      assert.equal(await page.locator('.turn-block').count(), 106);
      assert.equal(await page.evaluate(() => CSS.highlights.has('message-find')), false);
      const showDetails = page.getByRole('button', { name: 'Show details', exact: true }); if (await showDetails.isVisible()) await showDetails.click();
      await page.waitForFunction(() => !document.querySelector('.timeline-minimap'));
      await page.getByRole('button', { name: 'Close details', exact: true }).click();
      await waitCount(106);
      const humans = makeTurns(4);
      turns = humans.flatMap(turn => {
        const child = { ...turn, id: randomUUID(), parent: turn.id, text: 'Internal agent request' };
        const room = { root: turn.id, generation: randomUUID(), active: false, stopped: false, round: 1, silent: null, note: null, quiet: [], messages: [{ seq: 1, kind: 'reply', turn: turn.id, agent: agent.id, name: agent.name, role: '', text: 'First room reply' }, { seq: 2, kind: 'reply', turn: child.id, agent: agent.id, name: agent.name, role: '', text: 'Final room reply' }] };
        return [{ ...turn, room }, { ...child, room }];
      });
      await open(true);
      await page.getByText(humans[3].text, { exact: true }).waitFor();
      await waitCount(4);
      assert.deepEqual(await page.locator('.transcript .human-bubble').allTextContents(), humans.map(turn => turn.text));
      await rail.focus(); await rail.press('Home'); await rail.press('Enter'); await aligned(humans[0].id);
      const position = await point(1); await page.mouse.move(position.x, position.y);
      await page.locator('.timeline-minimap__preview').getByText('Final room reply', { exact: true }).waitFor();
      await page.screenshot({ path: join(evidence, 'room-hover.png'), animations: 'disabled' });
    });
  } finally {
    await page.unroute(pattern, route);
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.evaluate(() => { document.documentElement.dataset.theme = 'dark'; });
    await agentChat();
  }
}

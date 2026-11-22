import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';

export async function detailsDeclutterSmoke({ page, check, origin, screenshot, gitAgent, gitName, models, baseline = false, existing = false }) {
  const json = (route, value) => route.fulfill({ contentType: 'application/json', body: JSON.stringify(value) });
  const created = !existing && await page.request.post(origin + '/api/agents', { headers: { Origin: origin }, data: { runtime: 'api', id: gitAgent, name: gitName, model: models[0], instructions: 'Help with the local Git workspace.' } });
  if (created) assert(created.ok(), await created.text());
  const computer = `**/api/agents/${gitAgent}/computer`;
  await page.route(computer, route => json(route, { available: true, enabled: true, state: 'off', revision: 1, requests: [], control: { owner: 'agent' } }));
  await page.reload();
  await page.getByRole('navigation', { name: 'Choose agent', exact: true }).getByRole('button', { name: new RegExp(`^${gitName} `) }).click();
  const show = page.getByRole('button', { name: 'Show details', exact: true }); if (await show.isVisible()) await show.click();
  const details = page.getByRole('complementary', { name: 'Conversation details', exact: true });
  await check('LB1 single panel tab row across themes and widths', async () => {
    for (const viewport of [{ width: 1440, height: 900 }, { width: 1024, height: 768 }]) {
      await page.setViewportSize(viewport);
      for (const size of ['default', 'compact']) for (const palette of ['minarbot', 'github', 'catppuccin', 'everforest', 'solarized', 'midnight']) for (const theme of ['dark', 'light']) {
        await page.evaluate(values => Object.assign(document.documentElement.dataset, values), { textSize: size, colorTheme: palette, theme });
        for (const tab of ['Chats', 'Files', 'Diff']) {
          await details.getByRole('tab', { name: tab === 'Diff' ? 'Files' : tab, exact: true }).click();
          if (tab === 'Diff') await details.getByRole('button', { name: /^Source control/ }).click();
          assert.equal(await details.getByRole('tablist').count(), 1);
          assert(await details.evaluate(el => el.scrollWidth <= el.clientWidth + 1));
          if (palette === 'minarbot' || palette === 'catppuccin' && theme === 'dark') await screenshot(`lb1-after-${tab.toLowerCase()}-${palette}-${theme}-${size}-${viewport.width}`);
        }
      }
    }
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.evaluate(() => Object.assign(document.documentElement.dataset, { textSize: 'default', colorTheme: 'minarbot', theme: 'dark' }));
  });
  if (!baseline) {
    await check('LB1 profile resources, live summary and return focus', async () => {
      await details.getByRole('tab', { name: 'Chats', exact: true }).click();
      await details.getByRole('button', { name: 'Edit profile', exact: true }).click();
      const profile = page.getByRole('dialog', { name: 'Agent profile', exact: true });
      await profile.getByRole('tab', { name: 'Advanced', exact: true }).click();
      await profile.locator('.summary-row').scrollIntoViewIfNeeded(); await profile.getByText('None yet', { exact: true }).waitFor();
      assert.equal(await profile.getByRole('button', { name: 'Reload summary', exact: true }).count(), 0);
      const summaryPath = `**/api/agents/${gitAgent}/compaction?*`;
      await page.route(summaryPath, route => json(route, { version: 'fixture', through: 2, summary: 'Saved decision: keep the panel calm.', source_count: 1, sources: [{ id: randomUUID(), seq: 2, status: 'done' }], next_after: null }));
      try {
        await page.evaluate(() => window.dispatchEvent(new Event('focus')));
        const summary = profile.getByRole('button', { name: 'History summary Up to date', exact: true }); await summary.waitFor();
        await summary.click();
        const dialog = page.getByRole('dialog', { name: 'History summary', exact: true }); await dialog.getByText('Saved decision: keep the panel calm.', { exact: true }).waitFor();
        await page.waitForFunction(el => el.contains(document.activeElement), await dialog.elementHandle());
        await page.keyboard.press('Escape'); await dialog.waitFor({ state: 'hidden' });
        await page.waitForFunction(el => document.activeElement === el, await summary.elementHandle());
      } finally { await page.unroute(summaryPath); }
      await profile.getByRole('button', { name: 'Agent sites', exact: true }).click();
      const sites = page.getByRole('dialog', { name: 'Agent sites', exact: true }); await sites.getByRole('button', { name: 'Create site', exact: true }).waitFor();
      await page.waitForFunction(el => el.contains(document.activeElement), await sites.elementHandle());
      await page.keyboard.press('Escape'); await sites.waitFor({ state: 'hidden' });
      await page.waitForFunction(el => document.activeElement === el, await profile.getByRole('button', { name: 'Agent sites', exact: true }).elementHandle());
      await profile.getByRole('button', { name: 'Agent memory', exact: true }).click();
      const memory = page.getByRole('dialog', { name: 'Agent memory', exact: true }); await memory.getByRole('heading', { name: 'Minarbot Memory', exact: true }).waitFor();
      await page.waitForFunction(el => el.contains(document.activeElement), await memory.elementHandle());
      await page.keyboard.press('Escape'); await memory.waitFor({ state: 'hidden' });
      await page.waitForFunction(el => document.activeElement === el, await profile.getByRole('button', { name: 'Agent memory', exact: true }).elementHandle());
      await profile.getByRole('button', { name: 'Cancel', exact: true }).click(); await profile.waitFor({ state: 'hidden' });
    });
    await check('LB1 Chats omits run history, keeps queue recovery, waiting and routines navigation', async () => {
      const base = `/api/agents/${gitAgent}`, now = new Date().toISOString();
      const running = { id: randomUUID(), seq: 1, agent: gitAgent, text: 'Running fixture', status: 'running', reply: null, error: null, paused: 0, revision: 1, no_update: 0, created: now };
      const first = { ...running, id: randomUUID(), seq: 2, text: 'First queued fixture', status: 'queued' }, second = { ...first, id: randomUUID(), seq: 3, text: 'Second queued fixture' };
      const delegated = { ...running, id: randomUUID(), seq: 4, text: 'Delegated fixture', status: 'done', reply: 'Ready', parent: running.id };
      const wait = { ...running, id: randomUUID(), seq: 5, text: 'Waiting fixture', paused: 1, events: [{ id: randomUUID(), event: 'Review ready', status: 'waiting', deadline: Date.now() + 600000 }] };
      let queue = [first, second], fail = true; const commands = [], routes = [];
      const route = async (pattern, handler) => { routes.push(pattern); await page.route(pattern, handler); };
      await route(new RegExp(`${base}/turns(?:\\?|$)`), r => json(r, [running, ...queue, delegated, wait]));
      await route(`**${base}/dispatch`, r => json(r, [{ id: running.id, agent: gitAgent, capabilities: { steer: true, queue: true, restart: true } }]));
      await route(`**${base}/queue`, r => {
        if (r.request().method() === 'POST') { commands.push(r.request().postDataJSON()); if (fail) return r.fulfill({ contentType: 'application/json', body: '{' }); queue = [...queue].reverse(); return json(r, {}); }
        return json(r, { turns: queue.map((row, index) => ({ ...row, editing: false, previous: index ? { id: queue[index-1].id, agent: gitAgent, revision: 1, editing: false } : null, next: index < queue.length-1 ? { id: queue[index+1].id, agent: gitAgent, revision: 1, editing: false } : null })), more: false, next_after: null, snapshot: 'fixture' });
      });
      await route(`**${base}/routines`, r => json(r, [{ id: randomUUID(), name: 'Next review', enabled: 1, trigger: 'schedule', next_at: Date.now() + 3600000 }]));
      try {
        await page.reload();
        await page.getByRole('navigation', { name: 'Choose agent', exact: true }).getByRole('button', { name: new RegExp(`^${gitName} `) }).click();
        await details.getByRole('tab', { name: 'Chats', exact: true }).click();
        assert.equal(await details.getByRole('button', { name: /^Queue / }).count(), 0);
        for (const name of ['Waiting 1']) await details.getByRole('button', { name, exact: true }).waitFor();
        for (const name of ['Running fixture', 'Delegated fixture']) assert.equal(await details.getByRole('button', { name, exact: true }).count(), 0);
        await page.getByRole('button', { name: 'Order', exact: true }).click();
        const order = page.getByRole('dialog', { name: 'Queue order', exact: true });
        await order.getByRole('button', { name: 'Queue 2', exact: true }).waitFor();
        assert.equal(await details.locator('.activity-empty:visible').count(), 0);
        await order.getByRole('button', { name: 'Queue 2', exact: true }).click();
        await order.getByRole('button', { name: 'Move down', exact: true }).first().click();
        await order.getByRole('button', { name: 'Retry order', exact: true }).waitFor();
        await page.reload();
        await page.getByRole('navigation', { name: 'Choose agent', exact: true }).getByRole('button', { name: new RegExp(`^${gitName} `) }).click();
        await details.getByRole('tab', { name: 'Chats', exact: true }).click();
        await page.getByRole('button', { name: 'Order', exact: true }).click();
        await order.getByRole('button', { name: /^Queue 2 Recovery needed$/ }).click();
        await order.getByRole('button', { name: 'Retry order', exact: true }).waitFor();
        fail = false; await order.getByRole('button', { name: 'Retry order', exact: true }).click();
        await order.getByRole('button', { name: 'Retry order', exact: true }).waitFor({ state: 'hidden' });
        assert.equal(commands.length, 2); assert.deepEqual(commands[0], commands[1]);
        assert.equal(await details.getByRole('button', { name: 'Delegated fixture', exact: true }).count(), 0);
        await page.keyboard.press('Escape');
        await details.getByRole('button', { name: 'Waiting 1', exact: true }).click(); await details.getByRole('button', { name: 'Show event connection', exact: true }).waitFor();
        for (const label of ['Waiting 1']) { const row = details.getByRole('button', { name: label, exact: true }); if (await row.getAttribute('aria-expanded') === 'true') await row.click(); }
        for (const viewport of [{ width: 1440, height: 900 }, { width: 1024, height: 768 }]) {
          await page.setViewportSize(viewport);
          for (const size of ['default', 'compact']) for (const [palette, theme] of [['minarbot', 'dark'], ['minarbot', 'light'], ['catppuccin', 'dark']]) {
            await page.evaluate(({ size, palette, theme }) => Object.assign(document.documentElement.dataset, { textSize: size, colorTheme: palette, theme }), { size, palette, theme });
            await details.getByRole('tabpanel', { name: 'Chats', exact: true }).evaluate(el => { el.scrollTop = 0; });
            assert(await details.getByRole('tabpanel', { name: 'Chats', exact: true }).evaluate(el => el.scrollWidth <= el.clientWidth + 1));
            await screenshot(`lb1-activity-populated-${palette}-${theme}-${size}-${viewport.width}`);
          }
        }
        await page.setViewportSize({ width: 1440, height: 900 });
        await page.evaluate(() => Object.assign(document.documentElement.dataset, { textSize: 'default', colorTheme: 'minarbot', theme: 'dark' }));
        await details.getByRole('button', { name: /^Upcoming / }).click(); await details.getByText('Next review', { exact: true }).waitFor();
        await details.getByRole('button', { name: 'Manage', exact: true }).click(); await page.getByRole('heading', { name: 'Routines', exact: true }).waitFor();
      } finally { for (const pattern of routes) await page.unroute(pattern); await page.reload(); }
    });
  }
  await page.unroute(computer);
}

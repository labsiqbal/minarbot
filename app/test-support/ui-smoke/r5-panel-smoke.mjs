import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir } from 'node:fs/promises';
import { join } from 'node:path';

export async function r5PanelSmoke({ check, page, origin, root, agentChat, settings, openNew, gitAgent, gitName, reviewer }) {
  const evidence = join(root, 'app/test-support/ui-smoke/artifacts/ux-r5-panel/after');
  await mkdir(evidence, { recursive: true });
  const shot = name => page.screenshot({ path: join(evidence, name + '.png'), animations: 'disabled' });
  const details = page.getByRole('complementary', { name: 'Conversation details', exact: true });
  const open = async () => { const show = page.getByRole('button', { name: 'Show details', exact: true }); if (await show.isVisible()) await show.click(); await details.waitFor(); };
  const close = async () => { await page.getByRole('button', { name: 'Close settings', exact: true }).click(); };
  await check('R5B Settings headers share geometry, no refresh icons, MCP/data refresh on focus', async () => {
    for (const width of [1440, 1024]) {
      await page.setViewportSize({ width, height: 900 });
      let expected;
      for (const name of ['General', 'Appearance', 'Skills', 'Providers', 'Connected apps', 'MCP connections', 'Machines', 'Updates', 'Usage', 'Shared data', 'Backup & restore', 'External access']) {
        const dialog = await settings(name);
        const header = dialog.locator('.app-settings__page > .k-page > .k-page-head');
        await header.getByRole('heading', { name, exact: true }).waitFor();
        const geometry = await header.evaluate(el => {
          const rect = el.getBoundingClientRect(), actions = el.querySelector('.k-page-head__actions');
          return { x: rect.x, y: rect.y, width: rect.width, right: rect.right, action: actions?.getBoundingClientRect().right,
            buttons: [...(actions?.querySelectorAll('button') ?? [])].filter(button => button.getClientRects().length).map(button => button.getBoundingClientRect().height), subtitle: el.querySelectorAll('.k-page-head__text p').length };
        });
        expected ??= { x: geometry.x, y: geometry.y, width: geometry.width };
        for (const key of ['x', 'y', 'width']) assert(Math.abs(geometry[key] - expected[key]) < 1, JSON.stringify({ name, width, geometry, expected }));
        assert(geometry.width <= 800, JSON.stringify(geometry));
        assert.equal(geometry.subtitle, 0, name);
        if (geometry.action) assert(Math.abs(geometry.action - geometry.right) < 1, JSON.stringify(geometry));
        assert(geometry.buttons.every(height => height === 32), JSON.stringify({ name, geometry }));
        assert.equal(await dialog.locator('button.k-icon-btn').filter({ has: page.locator('svg.lucide-refresh-cw, svg.lucide-rotate-cw') }).count(), 0, name);
        assert.equal(await dialog.locator('button.k-icon-btn').and(dialog.getByRole('button', { name: /Refresh|Reload/i })).count(), 0, name);
        if (['MCP connections', 'Shared data'].includes(name)) {
          const pathname = name === 'Shared data' ? '/api/data' : '/api/mcp';
          const refreshed = page.waitForResponse(response => new URL(response.url()).pathname === pathname);
          await page.evaluate(() => window.dispatchEvent(new Event('focus'))); await refreshed;
        }
        if (['MCP connections', 'Machines', 'Shared data'].includes(name)) await shot(`settings-${name.toLowerCase().replaceAll(' ', '-')}-${width}`);
      }
    }
    await close(); await page.setViewportSize({ width: 1440, height: 900 });
  });
  await check('R5B Activity absent, agent menu orders and icons, file preview keeps width and one header', async () => {
    await agentChat(); await open();
    assert.equal(await details.getByRole('tab', { name: 'Activity', exact: true }).count(), 0);
    await details.getByRole('tab', { name: 'Chats', exact: true }).click();
    const upcoming = details.locator('.upcoming-row .k-collapsible__title');
    if (await upcoming.count()) assert.equal(await upcoming.evaluate(el => { const range = document.createRange(); range.selectNode(el.firstChild); return range.getClientRects().length; }), 1, 'Upcoming label wraps');
    await shot('activity');
    await page.locator('.chat__pill').click();
    const menu = page.getByRole('menu');
    const labels = await menu.getByRole('menuitem').allTextContents();
    assert.equal(labels[0], 'New conversation'); assert.equal(labels.at(-1), 'Export template'); assert(labels.includes('Hide panel'));
    for (const item of await menu.getByRole('menuitem').all()) assert.equal(await item.locator('svg').count(), 1, await item.innerText());
    assert.equal(await menu.getByRole('menuitem', { name: 'Export template', exact: true }).evaluate(el => getComputedStyle(el).borderTopStyle), 'solid');
    await shot('menu'); await page.keyboard.press('Escape');
    const directory = await page.request.get(origin + '/api/agents').then(response => response.json());
    if (!directory.agents.some(agent => agent.id === gitAgent)) {
      const created = await page.request.post(origin + '/api/agents', { headers: { Origin: origin }, data: { runtime: 'api', id: gitAgent, name: gitName, model: directory.defaultModel, instructions: 'Help with the local Git workspace.' } });
      assert(created.ok()); await page.reload();
    }
    await page.getByRole('navigation', { name: 'Choose agent', exact: true }).getByRole('button', { name: new RegExp(`^${gitName} `) }).click();
    await open(); await details.getByRole('tab', { name: 'Files', exact: true }).click();
    const width = (await details.boundingBox()).width;
    const file = details.getByRole('button', { name: /fixture\.html/ }).first();
    await file.click(); await details.getByRole('button', { name: 'Close file', exact: true }).waitFor();
    assert.equal((await details.boundingBox()).width, width);
    assert.equal(await details.getByRole('button', { name: /^Source control/ }).isVisible(), false);
    assert.equal(await details.locator('.right-panel__back:visible').count(), 0);
    assert.equal(await details.getByRole('link', { name: 'Download original', exact: true }).count(), 1);
    await details.frameLocator('iframe').getByRole('heading', { name: 'Synthetic preview', exact: true }).waitFor();
    await shot('preview'); await details.getByRole('button', { name: 'Close file', exact: true }).click();
    const upload = await page.request.post(origin + `/api/agents/${gitAgent}/files/${randomUUID()}`, { headers: { Origin: origin, 'Content-Type': 'application/octet-stream', 'X-File-Name': 'artifact.html' }, data: '<!doctype html><title>Artifact</title><h1>Synthetic artifact</h1>' });
    assert(upload.ok(), await upload.text());
    await page.evaluate(() => window.dispatchEvent(new Event('focus')));
    await details.getByRole('button', { name: 'Preview artifacts', exact: true }).click();
    assert.equal((await details.boundingBox()).width, width);
    assert.equal(await details.getByRole('button', { name: 'Back to files', exact: true }).count(), 1);
    assert.equal(await details.locator('.right-panel__preview-head:visible').count(), 1);
    await details.frameLocator('iframe').getByRole('heading', { name: 'Synthetic artifact', exact: true }).waitFor();
    await shot('artifact-preview'); await agentChat();
  });
  if (reviewer) await check('R5B room-only agent has empty direct sidebar preview', async () => {
    const directory = await page.request.get(origin + '/api/agents').then(response => response.json());
    const agent = directory.agents.find(agent => agent.name === reviewer); assert(agent);
    const turns = await page.request.get(origin + `/api/agents/${agent.id}/turns`).then(response => response.json());
    assert.equal(turns.length, 0);
    const summary = await page.request.get(origin + '/api/sidebar/summary').then(response => response.json());
    assert.deepEqual(summary.summaries.find(item => item.agent === agent.id), { agent: agent.id, preview: '', time: null, status: 'idle', needs_you: 0 });
    await page.getByRole('navigation', { name: 'Choose agent', exact: true }).getByRole('button', { name: new RegExp(`^${reviewer} `) }).click();
    await page.getByRole('main', { name: `Conversation ${reviewer}`, exact: true }).waitFor();
    await shot('sidebar-preview'); await agentChat();
  });
  await check('R6 palette keeps input focus without outer or input ring', async () => {
    await page.keyboard.press('Control+k'); const input = page.getByRole('combobox', { name: 'Jump to', exact: true }); await input.waitFor();
    const focus = await input.evaluate(el => { const input = getComputedStyle(el), frame = getComputedStyle(el.closest('[role=dialog]')); return { active: document.activeElement === el, outline: input.outlineStyle, shadow: input.boxShadow, frame: frame.outlineStyle }; });
    assert(focus.active); assert.equal(focus.outline, 'none'); assert.equal(focus.shadow, 'none'); assert.equal(focus.frame, 'none');
    await shot('palette'); await page.keyboard.press('Escape');
  });
  await check('R5B avatar preview matches typed name and final theme colors', async () => {
    const dialog = await openNew('New agent');
    await dialog.getByLabel('Name', { exact: true }).fill('Nova');
    const initial = dialog.locator('.agent-profile__initials .bs-avatar'); assert.equal(await initial.innerText(), 'N');
    const colors = el => { const style = getComputedStyle(el); return [style.backgroundColor, style.color]; };
    assert.deepEqual(await initial.evaluate(colors), await page.locator('.bot-row .bs-avatar').first().evaluate(colors));
    await shot('avatar'); await dialog.getByRole('button', { name: 'Change', exact: true }).click();
    const picker = page.getByRole('dialog', { name: 'Avatar', exact: true });
    assert.equal(await picker.locator('.k-avatar-picker__preview').innerText(), 'N');
    await picker.getByRole('button', { name: 'Done', exact: true }).click();
    await dialog.getByLabel('Name', { exact: true }).fill('  iris'); assert.equal(await initial.innerText(), 'I');
    await dialog.getByRole('button', { name: 'Cancel', exact: true }).click();
  });
  await check('R5B narrow sidebar off-canvas, keyboard dismissal and desktop width restored', async () => {
    await agentChat();
    const sidebar = page.getByRole('complementary', { name: 'Workspace navigation', exact: true });
    const originalWidth = (await sidebar.boundingBox()).width;
    for (const width of [767, 700]) {
      await page.setViewportSize({ width, height: 900 });
      const reveal = page.getByRole('button', { name: 'Expand sidebar', exact: true }); await reveal.waitFor();
      assert.equal(await sidebar.count(), 0);
      assert((await page.getByRole('main').boundingBox()).width >= width - 1);
      await shot(`narrow-${width}`); await reveal.click();
      const drawer = page.getByRole('dialog', { name: 'Workspace navigation', exact: true }); await drawer.waitFor();
      assert((await drawer.boundingBox()).width <= width - 48); await shot(`sidebar-${width}`);
      await page.waitForFunction(() => document.activeElement?.getAttribute('aria-label') === 'Workspace navigation'); await page.keyboard.press('Escape'); await drawer.waitFor({ state: 'hidden' }); await page.waitForFunction(() => document.activeElement?.getAttribute('aria-label') === 'Expand sidebar');
    }
    await page.getByRole('button', { name: 'Expand sidebar', exact: true }).click();
    const drawer = page.getByRole('dialog', { name: 'Workspace navigation', exact: true }); await drawer.waitFor();
    await drawer.locator('.bot-row').first().click(); await drawer.waitFor({ state: 'hidden' });
    await page.setViewportSize({ width: 768, height: 900 }); await sidebar.waitFor();
    await page.setViewportSize({ width: 1440, height: 900 }); assert.equal((await sidebar.boundingBox()).width, originalWidth);
  });
  await check('R5B all palette links visibly underlined in rendered prose', async () => {
    const theme = await page.evaluate(() => ({ ...document.documentElement.dataset }));
    await page.evaluate(() => {
      const prose = document.createElement('div'); prose.id = 'r5-link-fixture'; prose.className = 'markdown';
      const p = document.createElement('p'); p.textContent = 'Synthetic reference: ';
      const a = document.createElement('a'); a.href = '#synthetic'; a.textContent = 'Project notes'; p.append(a); prose.append(p);
      document.querySelector('.chat .transcript').append(prose);
    });
    try {
      for (const palette of ['minarbot', 'github', 'catppuccin', 'everforest', 'solarized', 'midnight']) for (const mode of ['light', 'dark']) {
        await page.evaluate(({ palette, mode }) => Object.assign(document.documentElement.dataset, { colorTheme: palette, theme: mode }), { palette, mode });
        const link = page.locator('#r5-link-fixture a'); await link.scrollIntoViewIfNeeded();
        const style = await link.evaluate(el => { const s = getComputedStyle(el); return { decoration: s.textDecorationLine, color: s.color, body: getComputedStyle(el.parentElement).color }; });
        assert(style.decoration.includes('underline'), JSON.stringify({ palette, mode, style })); assert.notEqual(style.color, style.body);
        await shot(`links-${palette}-${mode}`);
      }
    } finally { await page.evaluate(theme => { document.querySelector('#r5-link-fixture')?.remove(); Object.assign(document.documentElement.dataset, theme); }, theme); }
  });
}

import assert from 'node:assert/strict';

export async function widePanelsSmoke({ check, page, agentChat, visible, hidden, screenshot, viewport }) {
  const sidebar = page.getByRole('complementary', { name: 'Workspace navigation', exact: true });
  const details = page.getByRole('complementary', { name: 'Conversation details', exact: true });
  const left = sidebar.getByRole('separator', { name: 'Resize sidebar', exact: true });
  const right = details.getByRole('separator', { name: 'Resize details', exact: true });
  async function settings(section) {
    const dialog = page.getByRole('dialog', { name: 'Settings', exact: true });
    if (!await dialog.isVisible()) await page.getByRole('button', { name: 'Settings', exact: true }).click();
    await dialog.getByRole('navigation', { name: 'Settings', exact: true }).getByRole('button', { name: section, exact: true }).click();
    await visible(dialog.getByRole('heading', { name: section, level: 1, exact: true }));
    return dialog;
  }
  async function openPanel() {
    const show = page.getByRole('button', { name: 'Show details', exact: true });
    if (await show.isVisible()) await show.click();
    await visible(details);
  }
  async function settled() {
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.evaluate(() => Promise.all(document.getAnimations().filter(a => a.effect?.getTiming().iterations !== Infinity).map(a => a.finished.catch(() => {}))));
  }
  async function layout() {
    await settled();
    return page.evaluate(() => {
      const chat = document.querySelector('.chat-row > .chat');
      const panel = document.querySelector('.right-panel');
      return { main: chat.getBoundingClientRect().width, panel: panel?.getBoundingClientRect().width ?? 0,
        sidebar: document.querySelector('.sidebar').getBoundingClientRect().width,
        overflow: [document.documentElement, chat, chat.querySelector('.transcript'), chat.querySelector('.composer'), panel].filter(Boolean).some(el => el.scrollWidth > el.clientWidth + 1) };
    });
  }
  await check('RS1 right panel preserves chat and saved width across narrow windows', async () => {
    await agentChat(); await openPanel();
    await page.setViewportSize(viewport); await right.dblclick();
    const box = await right.boundingBox();
    await page.mouse.move(box.x + box.width / 2, 450); await page.mouse.down();
    await page.mouse.move(box.x - 16, 450, { steps: 4 }); await page.mouse.up();
    assert(Number(await right.getAttribute('aria-valuenow')) > 360, 'Pointer drag did not resize inline panel');
    await right.dblclick();
    await left.focus(); await page.keyboard.press('Home'); await page.keyboard.press('ArrowRight');
    assert.equal(await left.getAttribute('aria-valuenow'), '256'); await left.dblclick();
    await right.focus(); await page.keyboard.press('Home'); await page.keyboard.press('ArrowLeft');
    assert.equal(await right.getAttribute('aria-valuenow'), '296');
    await right.dblclick(); assert.equal(await right.getAttribute('aria-valuenow'), '360');
    for (const width of [1024, 900]) {
      await page.setViewportSize({ width, height: 900 }); await settled();
      const bounds = await layout();
      assert(bounds.main >= width - 301 && !bounds.overflow, JSON.stringify(bounds));
      assert.equal(await page.getByRole('dialog', { name: 'Conversation panel', exact: true }).count(), 1);
      await screenshot(`rs1-overlay-${width}`);
    }
    await page.setViewportSize(viewport); await settled();
    assert.equal(await right.getAttribute('aria-valuenow'), '360');
    await page.reload(); await openPanel(); assert.equal(await right.getAttribute('aria-valuenow'), '360');
  });
  await check('RS1 settings icons, search, keyboard and filled current destination', async () => {
    const dialog = await settings('Appearance');
    const nav = dialog.getByRole('navigation', { name: 'Settings', exact: true });
    const destinations = ['General', 'Appearance', 'Skills', 'Providers', 'Engines', 'Connected apps', 'Email', 'MCP connections', 'Vault', 'Machines', 'Devices', 'Computer', 'Updates', 'Usage', 'Shared data', 'Backup & restore', 'External access'];
    assert.deepEqual(await nav.getByRole('button').allTextContents(), destinations);
    assert(await nav.getByRole('button').evaluateAll(buttons => buttons.every(button => button.firstElementChild?.tagName.toLowerCase() === 'svg' && button.firstElementChild.getAttribute('aria-hidden') === 'true')));
    const selected = nav.getByRole('button', { name: 'Appearance', exact: true });
    assert.equal(await selected.getAttribute('aria-current'), 'page');
    await page.waitForFunction(() => { const el = document.querySelector('.app-settings__nav button[aria-current=page]'); return !!el && getComputedStyle(el).backgroundColor !== 'rgba(0, 0, 0, 0)' && parseFloat(getComputedStyle(el).borderRadius) > 0; });
    await selected.focus(); await page.keyboard.press('/');
    const search = dialog.getByRole('searchbox', { name: 'Search settings', exact: true });
    assert(await search.evaluate(el => el === document.activeElement));
    await search.fill('CONNECTIONS mcp');
    assert.deepEqual(await nav.getByRole('button').allTextContents(), ['MCP connections']);
    await page.keyboard.press('ArrowDown');
    assert(await nav.getByRole('button', { name: 'MCP connections', exact: true }).evaluate(el => el === document.activeElement));
    await page.keyboard.press('Enter');
    assert.equal(await nav.getByRole('button', { name: 'MCP connections', exact: true }).getAttribute('aria-current'), 'page');
    // Selection updates before the lazy destination mounts and focuses its heading.
    await page.waitForFunction(() => document.activeElement?.matches('.app-settings__page h1') && document.activeElement.textContent === 'MCP connections');
    await search.fill('not-a-setting'); await visible(nav.getByRole('status')); await screenshot('rs1-settings-search-empty');
    await search.focus(); await page.keyboard.press('Escape'); await visible(dialog); assert.equal(await search.inputValue(), '');
    await page.keyboard.press('ArrowDown'); await page.keyboard.press('End');
    assert(await nav.getByRole('button', { name: 'External access', exact: true }).evaluate(el => el === document.activeElement));
    await page.keyboard.press('ArrowDown');
    assert(await nav.getByRole('button', { name: 'General', exact: true }).evaluate(el => el === document.activeElement));
    await page.keyboard.press('ArrowUp');
    assert(await nav.getByRole('button', { name: 'External access', exact: true }).evaluate(el => el === document.activeElement));
    await page.keyboard.press('Home');
    for (const name of destinations.slice(1)) {
      await page.keyboard.press('ArrowDown');
      assert(await nav.getByRole('button', { name, exact: true }).evaluate(el => el === document.activeElement), `ArrowDown must reach ${name}`);
    }
    for (const name of destinations.slice(0, -1).reverse()) {
      await page.keyboard.press('ArrowUp');
      assert(await nav.getByRole('button', { name, exact: true }).evaluate(el => el === document.activeElement), `ArrowUp must reach ${name}`);
    }
    await search.fill('connections'); await page.keyboard.press('ArrowDown');
    assert(await nav.getByRole('button', { name: 'Providers', exact: true }).evaluate(el => el === document.activeElement));
    await page.keyboard.press('End');
    assert(await nav.getByRole('button', { name: 'Vault', exact: true }).evaluate(el => el === document.activeElement));
    await page.keyboard.press('Home'); await page.keyboard.press('ArrowUp');
    assert(await nav.getByRole('button', { name: 'Vault', exact: true }).evaluate(el => el === document.activeElement));
    await search.fill(''); await page.keyboard.press('ArrowDown');
    await page.keyboard.press('Home'); await page.keyboard.press('ArrowDown'); await page.keyboard.press('Enter');
    await visible(dialog.getByRole('heading', { name: 'Appearance', exact: true }));
    await search.fill('/'); assert.equal(await search.inputValue(), '/'); // Editable slash stays text.
    await search.fill(''); await dialog.getByRole('button', { name: 'Close settings', exact: true }).click();
  });
  await check('RS1 Compact density, persistence and dark light color screenshots', async () => {
    await agentChat(); await openPanel();
    await details.getByRole('tab', { name: 'Chats', exact: true }).click();
    const metrics = () => page.evaluate(() => {
      const css = selector => { const el = document.querySelector(selector); const s = getComputedStyle(el); return { font: parseFloat(s.fontSize), height: el.getBoundingClientRect().height, padding: parseFloat(s.paddingTop), gap: parseFloat(s.gap) }; };
      return { name: css('.bot-row__name'), row: css('.bot-row'), tab: css('.sidebar__tabs .k-tabs__tab'), search: css('.sidebar__search input'), panel: css('.right-panel__header'), chat: css('.composer__input') };
    });
    const baseline = await metrics();
    let dialog = await settings('Appearance');
    await dialog.getByRole('radio', { name: 'Compact', exact: true }).click();
    assert.equal(await page.evaluate(() => JSON.parse(localStorage.getItem('minarbot-presentation-v1')).textSize), 'compact');
    await dialog.getByRole('button', { name: 'Close settings', exact: true }).click();
    const compact = await metrics();
    for (const key of ['name', 'search', 'chat']) assert(Math.abs(compact[key].font / baseline[key].font - .875) < .001, `${key}: ${JSON.stringify({baseline, compact})}`);
    assert(compact.row.height < baseline.row.height && compact.row.height >= 40);
    assert(compact.row.padding < baseline.row.padding && compact.tab.height < baseline.tab.height && compact.panel.height < baseline.panel.height);
    await page.reload(); await openPanel(); assert.equal(await page.evaluate(() => document.documentElement.dataset.textSize), 'compact');
    for (const width of [1440, 1024]) {
      await page.setViewportSize({ width, height: 900 }); await settled();
      const sheet = page.getByRole('dialog', { name: 'Conversation panel', exact: true });
      if (await sheet.isVisible()) { await sheet.getByRole('button', { name: 'Close details', exact: true }).click(); await hidden(sheet); }
      await left.focus(); await page.keyboard.press('Home');
      for (const [colorTheme, theme] of [['minarbot', 'dark'], ['minarbot', 'light'], ['catppuccin', 'dark']]) {
        dialog = await settings('Appearance');
        await dialog.getByRole('button', { name: colorTheme === 'minarbot' ? 'Minarbot' : 'Catppuccin', exact: true }).click();
        await dialog.getByRole('radio', { name: theme === 'dark' ? 'Dark' : 'Light', exact: true }).click();
        assert(await dialog.evaluate(el => el.scrollWidth <= el.clientWidth + 1));
        await screenshot(`rs1-compact-settings-${colorTheme}-${theme}-${width}`);
        await dialog.getByRole('button', { name: 'Close settings', exact: true }).click();
        const bounds = await layout(); assert(!bounds.overflow && bounds.main >= 359, JSON.stringify(bounds));
        await screenshot(`rs1-compact-chat-${colorTheme}-${theme}-${width}`);
      }
    }
    await page.setViewportSize(viewport); dialog = await settings('Appearance');
    await dialog.getByRole('radio', { name: 'Default', exact: true }).click();
    await dialog.getByRole('button', { name: 'Minarbot', exact: true }).click();
    await dialog.getByRole('radio', { name: 'Dark', exact: true }).click();
    await dialog.getByRole('button', { name: 'Close settings', exact: true }).click();
    await left.dblclick(); await openPanel(); await right.dblclick();
  });
}

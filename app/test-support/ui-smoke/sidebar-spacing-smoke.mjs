import assert from 'node:assert/strict';

export async function sidebarSpacingSmoke({ page, check, sidebarTab, screenshot, projectName, baseline = false }) {
  await check(baseline ? 'SB1 baseline project row visible' : 'SB1 folder disclosure and keyboard access', async () => {
    await sidebarTab('Projects');
    const row = page.getByRole('navigation', { name: 'Projects', exact: true }).getByRole('button', { name: projectName, exact: true });
    await row.waitFor();
    if (baseline) return;
    const project = row.locator('..').locator('..');
    const expanded = async value => {
      assert.equal(await project.getByRole('button', { name: `${value ? 'Collapse' : 'Expand'} project ${projectName}`, exact: true }).getAttribute('aria-expanded'), String(value));
      assert.equal(await row.locator(value ? '.lucide-folder-open' : '.lucide-folder').count(), 1);
      assert.equal(await project.locator('.project-navigation__room').count() > 0 && await project.locator('.project-navigation__room').first().isVisible(), value);
    };
    assert.equal(await project.getByRole('button', { name: `Collapse project ${projectName}`, exact: true }).count(), 1);
    await row.click();
    await page.getByRole('heading', { name: projectName, level: 1, exact: true }).waitFor();
    await expanded(true);
    await project.getByRole('button', { name: `Collapse project ${projectName}`, exact: true }).click(); await expanded(false);
    await page.reload(); await row.waitFor(); await expanded(false);
    await project.getByRole('button', { name: `Expand project ${projectName}`, exact: true }).click(); await expanded(true);
    await page.getByRole('textbox', { name: 'Search projects', exact: true }).fill('general');
    await expanded(true);
    await page.getByRole('textbox', { name: 'Search projects', exact: true }).fill('');
    const menu = page.getByRole('button', { name: `Menu project ${projectName}`, exact: true });
    await menu.click(); await page.getByRole('menuitem', { name: 'Project settings', exact: true }).click();
    await page.getByRole('heading', { name: projectName, level: 1, exact: true }).waitFor();
    await expanded(true);
  });
  await check('SB1 sidebar spacing screenshot matrix', async () => {
    await sidebarTab('Agents');
    await page.locator('.bot-row').first().click();
    const measurements = [];
    const sidebar = page.getByRole('complementary', { name: 'Workspace navigation', exact: true });
    for (const viewport of [{ width: 1440, height: 900 }, { width: 1024, height: 768 }]) {
      await page.setViewportSize(viewport);
      for (const size of ['default', 'compact', 'large', 'larger']) for (const theme of ['dark', 'light']) for (const tab of ['Agents', 'Projects']) {
        await page.evaluate(({ size, theme }) => { document.documentElement.dataset.textSize = size; document.documentElement.dataset.theme = theme; }, { size, theme });
        await sidebarTab(tab);
        await page.getByRole('textbox', { name: `Search ${tab.toLowerCase()}`, exact: true }).focus();
        await page.mouse.move(viewport.width - 20, viewport.height - 20);
        const layout = await sidebar.evaluate(el => [...el.querySelectorAll('.bot-row, .project-navigation__name, .project-navigation__room')].filter(row => row.getClientRects().length).map(row => {
          const style = getComputedStyle(row), text = row.querySelector('.bot-row__text');
          return { kind: row.classList.contains('bot-row') ? 'agent' : row.classList.contains('project-navigation__name') ? 'project' : 'section', height: row.getBoundingClientRect().height, padding: style.paddingBlockStart, gap: style.gap, textGap: text ? getComputedStyle(text).gap : null, width: row.clientWidth, scroll: row.scrollWidth };
        }));
        measurements.push({ viewport, size, theme, tab, rows: layout });
        assert(layout.length > 0); assert(layout.every(row => row.scroll <= row.width + 1), JSON.stringify(layout));
        assert(await sidebar.locator('.sidebar__scroll').evaluate(el => el.scrollWidth <= el.clientWidth + 1), 'Sidebar overflow');
        const controls = sidebar.locator('.sidebar__titlebar button');
        assert.deepEqual(await controls.evaluateAll(buttons => buttons.map(button => button.getAttribute('aria-label'))), ['Routines', 'Workflows', 'Automation calendar', 'Create new', 'Collapse sidebar']);
        assert(await controls.evaluateAll(buttons => buttons.every((button, i) => button.getBoundingClientRect().width >= 40 && (!i || button.getBoundingClientRect().left >= buttons[i - 1].getBoundingClientRect().right))), 'Header controls overlap');
        if (!baseline) {
          const density = { default: 1, compact: .875, large: 1.125, larger: 1.25 }[size];
          assert(layout.every(row => Math.abs(row.height - (row.kind === 'agent' ? Math.max(44, 48 * density) : row.kind === 'project' ? Math.max(38, 42 * density) : Math.max(32, 36 * density))) < 1), JSON.stringify(layout));
          if (tab === 'Agents') {
            assert(layout.every(row => Math.abs(parseFloat(row.textGap) - 3 * density) < .1), JSON.stringify(layout));
            assert(await sidebar.locator('.bot-list').evaluateAll(lists => lists.every(el => Math.abs(parseFloat(getComputedStyle(el).gap) - 6 * parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--density'))) < .1)), 'Agent row gaps');
          } else {
            const menu = sidebar.getByRole('button', { name: `Menu project ${projectName}`, exact: true });
            assert.equal(await menu.evaluate(el => getComputedStyle(el).opacity), '0');
            await menu.focus(); assert.equal(await menu.evaluate(el => getComputedStyle(el).opacity), '1');
            await page.getByRole('textbox', { name: 'Search projects', exact: true }).focus();
          }
        }
        if (size === 'default' || size === 'compact') await screenshot(`sb1-${baseline ? 'before' : 'after'}-${tab}-${theme}-${size}-${viewport.width}x${viewport.height}`);
      }
    }
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.evaluate(() => { document.documentElement.dataset.textSize = 'default'; document.documentElement.dataset.theme = 'dark'; });
    await sidebarTab('Agents');
    return JSON.stringify(measurements);
  });
}

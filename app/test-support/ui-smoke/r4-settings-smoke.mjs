import assert from 'node:assert/strict';
import { mkdir } from 'node:fs/promises';
import { join } from 'node:path';

export async function r4SettingsSmoke({ check, page, origin, root, agentChat, settings, openNew }) {
  const errors = [], onConsole = event => { if (event.type() === 'error') errors.push({ message: event.text(), location: event.location() }); };
  page.on('console', onConsole);
  const evidence = join(root, 'app/test-support/ui-smoke/artifacts/ux-r4-settings/after');
  await mkdir(evidence, { recursive: true });
  const shot = async name => { await page.mouse.move(0, 0); await page.screenshot({ path: join(evidence, name + '.png'), animations: 'disabled' }); };
  const panel = page.getByRole('complementary', { name: 'Conversation details', exact: true });
  const openPanel = async () => { const show = page.getByRole('button', { name: 'Show details', exact: true }); if (await show.isVisible()) await show.click(); await panel.waitFor(); };
  const close = async dialog => { await dialog.getByRole('button', { name: /^(Close|Close settings)$/ }).first().click(); await dialog.waitFor({ state: 'hidden' }); };
  async function focusRings(scope) {
    await page.keyboard.press('Tab');
    const controls = scope.locator('button, a[href], input, textarea, summary, [tabindex], [contenteditable=true]');
    let checked = 0;
    for (const control of await controls.all()) {
      if (!await control.isVisible() || await control.isDisabled() || await control.evaluate(el => el.tabIndex < 0)) continue;
      const result = await control.evaluate(async el => {
        const parents = [el, el.parentElement, el.parentElement?.parentElement, el.closest('.k-dialog:has(.quick-switcher)')].filter(Boolean);
        const before = parents.map(node => { const s = getComputedStyle(node); return [s.boxShadow, s.borderColor]; });
        el.focus();
        await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
        const ring = parents.some((node, i) => {
          const s = getComputedStyle(node);
          return s.outlineStyle !== 'none' && parseFloat(s.outlineWidth) >= 1 && s.outlineColor !== 'rgba(0, 0, 0, 0)' || s.boxShadow !== 'none' && s.boxShadow !== before[i][0] || s.borderColor !== before[i][1] && parseFloat(s.borderWidth) > 0;
        });
        const active = document.activeElement, style = active && getComputedStyle(active);
        const delegated = active !== el && el.contains(active) && (style?.outlineStyle !== 'none' && parseFloat(style?.outlineWidth) >= 1 || style?.boxShadow !== 'none');
        return { plainPaletteInput: el.matches('.quick-switcher__search input'), ring: ring || delegated, label: el.getAttribute('aria-label') ?? el.textContent?.trim().slice(0, 80), tag: el.tagName, focused: active === el || delegated };
      });
      assert(result.focused && (result.ring || result.plainPaletteInput), JSON.stringify(result)); checked++;
    }
    assert(checked > 0, 'No focusable controls inspected');
    await scope.evaluate(el => { const active = document.activeElement; if (active instanceof HTMLElement && el.contains(active)) active.blur(); });
    await page.waitForFunction(() => document.querySelectorAll('[role="tooltip"]').length === 0);
    return checked;
  }
  await check('R4B new agent defaults collapse, hidden validation reveals fields', async () => {
    const dialog = await openNew('New agent');
    for (const label of ['Run with', 'Provider', 'Model']) assert.equal(await dialog.getByRole('combobox', { name: label, exact: true }).isVisible(), false);
    await shot('R3-04-new-agent'); await focusRings(dialog);
    await dialog.getByRole('button', { name: 'Advanced settings', exact: true }).click();
    const model = dialog.getByRole('combobox', { name: 'Model', exact: true });
    const initial = await model.inputValue(); await model.fill(''); await page.keyboard.press('Escape');
    await dialog.getByRole('button', { name: 'Advanced settings', exact: true }).click();
    await dialog.getByLabel('Name', { exact: true }).fill('Validation fixture');
    await dialog.getByRole('button', { name: 'Create agent', exact: true }).click();
    assert.equal(await dialog.getByRole('button', { name: 'Advanced settings', exact: true }).getAttribute('aria-expanded'), 'true');
    assert(await model.evaluate(el => el === document.activeElement)); await model.fill(initial);
    await shot('R3-04-advanced'); await focusRings(dialog); await close(dialog);
  });
  await check('R4B Help has one title and visible keyboard focus', async () => {
    await page.getByRole('button', { name: 'Help', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: 'Keyboard shortcuts', exact: true }); await dialog.waitFor();
    assert.equal(await dialog.getByRole('heading', { name: 'Keyboard shortcuts', exact: true }).count(), 1);
    await shot('R3-05-help'); await focusRings(dialog); await close(dialog);
  });
  await check('R4B profile has three tabs, stable footer and aligned resource rows', async () => {
    await agentChat(); await openPanel(); await page.getByRole('button', { name: 'Edit profile', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: 'Agent profile', exact: true });
    assert.deepEqual(await dialog.getByRole('tab').allTextContents(), ['Profile', 'Model', 'Advanced']);
    for (const name of ['Profile', 'Model', 'Advanced']) {
      await dialog.getByRole('tab', { name, exact: true }).click();
      assert.deepEqual(await dialog.locator('.k-dialog__foot button').allTextContents(), ['Cancel', 'Save changes']);
      await focusRings(dialog); await shot(`R3-19-${name.toLowerCase()}`);
    }
    const rows = await dialog.locator('.agent-profile__resources .panel-row').evaluateAll(nodes => nodes.map(el => ({ x: el.querySelector('svg').getBoundingClientRect().x, text: el.querySelector('.panel-row__title').getBoundingClientRect().x })));
    assert.equal(rows.length, 3); assert(rows.every(row => row.x === rows[0].x && row.text === rows[0].text), JSON.stringify(rows));
    await shot('R3-20-resources'); await close(dialog);
  });
  await check('R4B Computer shares width with every tab and uses monitor icon', async () => {
    await agentChat(); await openPanel();
    const route = /\/api\/agents\/[^/]+\/computer$/;
    const status = { available: true, enabled: true, state: 'running', revision: 0, width: 1280, height: 800, requests: [], control: { mode: 'agent', revision: 0, since: new Date().toISOString() } };
    await page.route(route, r => r.fulfill({ json: status }));
    try {
      await panel.getByRole('tab', { name: 'Chats', exact: true }).click();
      await page.evaluate(() => window.dispatchEvent(new Event('focus')));
      await panel.getByRole('region', { name: 'Computer', exact: true }).waitFor();
      assert.equal(await panel.locator('.computer-compact-row img').count(), 0);
      assert.equal(await panel.locator('.computer-compact-row > svg').count(), 1);
      assert.equal(await panel.locator('.right-panel__header').innerText(), '');
      await shot('R3-18-computer-row'); status.state = 'stopped';
      const handle = panel.getByRole('separator', { name: 'Resize details', exact: true });
      await handle.focus(); await page.keyboard.press('ArrowLeft'); await page.keyboard.press('ArrowLeft');
      const width = (await panel.boundingBox()).width;
      for (const name of ['Computer', 'Files', 'Chats', 'Computer', 'Chats']) {
        await panel.getByRole('tab', { name, exact: true }).click(); assert(Math.abs((await panel.boundingBox()).width - width) < 0.01);
      }
      await focusRings(panel);
      await panel.getByRole('tab', { name: 'Computer', exact: true }).click(); await focusRings(panel); await shot('R3-17-computer');
      await page.reload(); await panel.waitFor(); assert(Math.abs((await panel.boundingBox()).width - width) < 0.01);
      await panel.getByRole('tab', { name: 'Chats', exact: true }).click(); assert(Math.abs((await panel.boundingBox()).width - width) < 0.01);
      await shot('R3-17-chats'); await handle.dblclick();
    } finally { await page.unroute(route); await page.reload(); }
  });
  await check('R4B Files, Diff and Preview share insets, compact rows and leading back buttons', async () => {
    await agentChat(); await openPanel();
    const agent = (await page.request.get(origin + '/api/agents').then(r => r.json())).agents.find(a => a.name === 'Smoke Lead') ?? (await page.request.get(origin + '/api/agents').then(r => r.json())).agents.find(a => a.name === 'Coder');
    const base = `/api/agents/${agent.id}/panel`, file = { id: 'r4-output', seq: 1, name: 'output.html', size: 42 };
    const pattern = new RegExp(base + '(?:/|$|\\?)');
    await page.route(pattern, route => {
      const url = new URL(route.request().url()), path = url.pathname.slice(base.length);
      if (url.searchParams.get('metadata') === '1') return route.fulfill({ json: { ...file, image: false } });
      if (url.searchParams.get('preview') === 'render') return route.fulfill({ contentType: 'text/html', body: '<h1>Synthetic preview</h1>' });
      const json = path === '' ? { workspace: { name: 'Fixture' }, computer: null, previews: [file], sites: [] } : path === '/files' ? [file] : path === '/git' ? { isRepo: true, branch: 'main', changedFiles: [{ code: ' M', path: 'README.md' }, { code: '??', path: 'draft.txt' }] } : path === '/git/diff' ? { text: '--- a/README.md\n+++ b/README.md\n@@ -1 +1 @@\n-old\n+new\n', truncated: false, binaryFiles: [] } : path === '/workspace/entries' ? { path: '', entries: [{ name: 'README.md', kind: 'file' }] } : { html: '<h1>Synthetic preview</h1>', text: 'Synthetic text', digest: 'fixture' };
      return route.fulfill({ json });
    });
    try {
      await page.evaluate(() => window.dispatchEvent(new Event('focus'))); await panel.getByRole('tab', { name: 'Files', exact: true }).click();
      const output = panel.getByRole('button', { name: 'Preview output.html', exact: true }); await output.waitFor();
      const workspace = panel.getByRole('button', { name: 'README.md', exact: true }); await workspace.waitFor();
      const source = panel.getByRole('button', { name: /^Source control/ });
      const positions = await Promise.all([output, workspace, source].map(el => el.boundingBox()));
      assert(positions.every(box => Math.abs(box.x - positions[0].x) < 1 && box.height >= 28 && box.height <= 32), JSON.stringify(positions));
      await shot('R3-15-files'); await focusRings(panel); await source.click();
      await panel.getByRole('region', { name: 'Unified diff', exact: true }).waitFor();
      assert.deepEqual(await panel.locator('.source-control__status').allTextContents(), ['M', 'U']);
      const back = panel.getByRole('button', { name: 'Back to files', exact: true });
      const backBox = await back.boundingBox(), title = await panel.locator('.right-panel__back > span').boundingBox(); assert(backBox.x < title.x);
      await shot('R3-15-diff'); await focusRings(panel); await back.click();
      await panel.getByRole('button', { name: 'Preview artifacts', exact: true }).click();
      await panel.frameLocator('iframe').getByRole('heading', { name: 'Synthetic preview', exact: true }).waitFor();
      await shot('R3-15-preview'); await focusRings(panel); await back.click(); await output.click();
      const fileBack = panel.getByRole('button', { name: 'Close file', exact: true });
      assert((await fileBack.boundingBox()).x < (await panel.locator('.right-panel__path', { hasText: 'output.html' }).boundingBox()).x);
      await shot('R3-15-file-preview'); await focusRings(panel); await fileBack.click();
    } finally { await page.goto('about:blank'); await page.unroute(pattern); await page.goto(origin); }
  });
  await check('R4B matching paired versions show Up to date without unavailable actions', async () => {
    const list = await page.request.get(origin + '/api/machines').then(r => r.json());
    const current = await page.request.get(origin + '/api/updates').then(r => r.json());
    const machine = { ...(list.machines[0] ?? {}), id: 'r4-peer', name: 'R4 paired fixture', version: current.current.version, allowUpdates: true, status: 'online', agents: [] };
    await page.route('**/api/updates', route => route.fulfill({ json: { ...current, available: undefined, feed: current.current } }));
    await page.route('**/api/machines', route => route.fulfill({ json: { machines: [machine] } }));
    await page.route('**/api/updates/machines', route => route.fulfill({ json: { machines: [{ machine: machine.id, available: false, reason: 'Enable updates on both machines.' }] } }));
    try {
      const dialog = await settings('Updates');
      const row = dialog.locator('.k-row').filter({ hasText: machine.name }); await row.getByText(/Up to date/).waitFor();
      assert.equal(await dialog.getByText('Enable updates on both machines.', { exact: true }).count(), 0);
      assert.equal(await row.getByRole('button').count(), 0); assert.equal(await row.getByRole('switch').count(), 0);
      await shot('R3-23-updates'); await close(dialog);
    } finally { await page.unroute('**/api/updates'); await page.unroute('**/api/machines'); await page.unroute('**/api/updates/machines'); }
  });
  await check('R4B Settings auto-refresh, edge alignment, peer status and theme geometry', async () => {
    for (const [name, key] of [['Skills', 'skills'], ['Connected apps', 'apps'], ['Machines', ''], ['External access', '']]) {
      const dialog = await settings(name); await dialog.getByRole('heading', { name, exact: true }).waitFor();
      const header = dialog.locator('.app-settings__page > .k-page > .k-page-head');
      const geometry = await header.evaluate(el => { const a = el.querySelector('.k-page-head__actions'); return { right: el.getBoundingClientRect().right, action: a?.getBoundingClientRect().right }; });
      if (geometry.action) assert(Math.abs(geometry.right - geometry.action) < 1, JSON.stringify({ name, ...geometry }));
      if (key) {
        assert.equal(await dialog.getByRole('button', { name: /Reload|Refresh/ }).count(), 0);
        await dialog.locator('.k-skeleton').first().waitFor({ state: 'hidden' });
        const path = key === 'skills' ? '/api/skills' : '/api/connected-apps/key';
        const refreshed = page.waitForRequest(request => new URL(request.url()).pathname === path);
        await page.evaluate(() => window.dispatchEvent(new Event('focus')));
        assert((await (await refreshed).response())?.ok(), `${name} refresh failed`);
      }
      if (name === 'Skills') assert.equal(await dialog.getByRole('button', { name: 'Import', exact: true }).evaluate(el => el.lastElementChild?.tagName), 'svg');
      if (name === 'External access') assert.equal(await dialog.getByText('Apps that can access Minarbot', { exact: true }).count(), 0);
      await focusRings(dialog); await shot(`R3-21-22-${key || name.toLowerCase().replace(' ', '-')}`);
      if (name === 'Skills') {
        await dialog.getByRole('tab', { name: 'Packs', exact: true }).click();
        await dialog.getByText('No packs', { exact: true }).waitFor();
        assert.equal(await dialog.getByRole('button', { name: /Reload|Refresh/ }).count(), 0);
        const refreshed = page.waitForResponse(r => new URL(r.url()).pathname === '/api/skill-packs');
        await page.evaluate(() => window.dispatchEvent(new Event('focus'))); await refreshed;
        await focusRings(dialog); await shot('R3-21-packs');
      }
    }
    const catalogRoute = /\/api\/connected-apps\?/;
    await page.route(catalogRoute, route => route.fulfill({ contentType: 'application/json', body: '{' }));
    try {
      const failed = await settings('Connected apps');
      await failed.getByRole('alert').waitFor();
      assert.equal(await failed.locator('.k-skeleton').count(), 0);
      await page.unroute(catalogRoute);
      await failed.getByRole('button', { name: 'Try again', exact: true }).click();
      await failed.getByRole('alert').waitFor({ state: 'hidden' });
    } finally { await page.unroute(catalogRoute); }
    const dialog = await settings('Appearance');
    for (const mode of ['dark', 'light']) {
      await dialog.getByRole('radiogroup', { name: 'Mode', exact: true }).getByRole('radio', { name: mode === 'dark' ? 'Dark' : 'Light', exact: true }).click();
      const cards = dialog.locator('.color-theme-card');
      const bounds = () => cards.evaluateAll(nodes => nodes.map(el => { const r = el.getBoundingClientRect(); const preview = el.firstElementChild.getBoundingClientRect(); return [r.x, r.y, r.width, r.height, preview.x - r.x, preview.y - r.y]; }));
      await cards.first().click(); const before = await bounds(); await cards.nth(1).click(); assert.deepEqual(await bounds(), before);
      assert.equal(await cards.getByText('Selected', { exact: true }).count(), 0); await shot(`R3-27-themes-${mode}`);
    }
    await dialog.locator('.color-theme-card').first().click(); await dialog.getByRole('radiogroup', { name: 'Mode', exact: true }).getByRole('radio', { name: 'Dark', exact: true }).click(); await focusRings(dialog); await close(dialog);
  });
  await check('R4B palette restores opener on Escape and composer after selection', async () => {
    await agentChat();
    const opener = page.getByRole('button', { name: 'Quick switcher (Ctrl/Cmd+K)', exact: true });
    await opener.click(); await page.getByRole('dialog', { name: 'Quick switcher', exact: true }).waitFor();
    await page.keyboard.press('Escape'); await page.waitForFunction(() => document.activeElement?.getAttribute('aria-label') === 'Quick switcher (Ctrl/Cmd+K)');
    const currentName = (await page.getByRole('main').getAttribute('aria-label')).replace('Conversation ', '');
    const agent = (await page.request.get(origin + '/api/agents').then(r => r.json())).agents.find(a => a.name !== currentName && !a.hidden);
    assert(agent);
    for (const choose of ['keyboard', 'pointer']) {
      await agentChat();
      await page.keyboard.press('Control+k'); const input = page.getByRole('combobox', { name: 'Jump to', exact: true });
      await input.fill(agent.name); await focusRings(page.getByRole('dialog', { name: 'Quick switcher', exact: true }));
      const line = await page.locator('.quick-switcher__search').evaluate(el => getComputedStyle(el).boxShadow); assert.equal(line, 'none');
      assert.equal(await page.getByRole('dialog', { name: 'Quick switcher', exact: true }).evaluate(el => getComputedStyle(el).outlineStyle), 'none');
      const threads = new RegExp(`/api/agents/${agent.id}/threads$`);
      let release; const gate = new Promise(resolve => { release = resolve; });
      await page.route(threads, async route => { await gate; await route.continue(); });
      try {
        if (choose === 'keyboard') await input.press('Enter'); else await page.getByRole('option', { name: agent.name, exact: true }).click();
        await page.getByText('Loading conversation...', { exact: true }).waitFor();
        await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(() => requestAnimationFrame(resolve)))));
        release();
        await page.waitForFunction(() => document.activeElement?.matches('.composer__input'));
      } finally { release(); await page.unroute(threads); }
      assert.equal(await page.getByRole('main').getAttribute('aria-label'), `Conversation ${agent.name}`);
      await shot(`R3-24-palette-${choose}`);
    }
    await agentChat(); await openPanel(); await panel.getByRole('tab', { name: 'Chats', exact: true }).click();
    const more = panel.getByRole('button', { name: 'Menu conversation Main chat', exact: true });
    await more.hover(); await page.getByRole('tooltip').getByText('More actions', { exact: true }).waitFor(); await shot('R3-25-tooltip');
    await page.mouse.move(0, 0); await page.keyboard.press('Escape');
    const count = await focusRings(page.locator('.app'));
    const handle = page.getByRole('separator', { name: 'Resize sidebar', exact: true }); await handle.focus(); await shot('R3-26-focus');
    assert.equal(await page.locator('.sidebar [role=tabpanel]').first().getAttribute('tabindex'), '-1');
    assert.deepEqual(errors, [], 'R4B browser health');
    return `${count} workspace controls show visible focus indicators.`;
  });
  page.off('console', onConsole);
}

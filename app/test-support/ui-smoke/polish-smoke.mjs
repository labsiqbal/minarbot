import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir } from 'node:fs/promises';
import { join } from 'node:path';

export async function polishSmoke({ check, page, origin, root, agentChat, providerAccount = 'st1-stream' }) {
  const evidence = join(root, 'app/test-support/ui-smoke/artifacts/ux-r3-polish/after');
  await mkdir(evidence, { recursive: true });
  const shot = name => page.screenshot({ path: join(evidence, `${name}.png`), animations: 'disabled' });
  const post = async (path, data) => {
    const response = await page.request.post(origin + path, { headers: { Origin: origin }, data });
    assert(response.ok(), await response.text()); return response.json();
  };
  const directory = await (await page.request.get(origin + '/api/agents')).json();
  const makeAgent = name => post('/api/agents', {runtime: 'api',  id: randomUUID(), name, model: directory.defaultModel, instructions: 'Synthetic polish check.', ...(providerAccount ? { providerAccount } : {}) });
  const iris = await makeAgent('R3C Iris'), theo = await makeAgent('R3C Theo'), mara = await makeAgent('R3C Mara');
  const project = await post('/api/projects', { id: randomUUID(), name: 'R3C Launch', description: '', folders: [], members: [{ agent: iris.id, write: false }, { agent: theo.id, write: false }, { agent: mara.id, write: false }] });
  const openAgent = async (name = 'R3C Iris') => {
    await page.reload(); await page.getByRole('tab', { name: /^Agents/ }).click();
    await page.getByRole('navigation', { name: 'Choose agent', exact: true }).getByRole('button', { name: new RegExp(`^${name} `) }).click();
  };
  const panel = page.getByRole('complementary', { name: 'Conversation details', exact: true });
  const showPanel = async () => { const button = page.getByRole('button', { name: 'Show details', exact: true }); if (await button.isVisible()) await button.click(); await panel.waitFor(); };
  const switcher = page.getByRole('dialog', { name: 'Quick switcher', exact: true });
  const choose = async name => {
    await page.keyboard.press('Control+k'); const input = switcher.getByRole('combobox'); await input.fill(name); await input.press('Enter'); await switcher.waitFor({ state: 'hidden' });
  };
  try {
    await check('R3C project home, separate disclosure, continuous selection and single room hash', async () => {
      await page.reload(); await page.getByRole('tab', { name: /^Projects/ }).click();
      const name = page.getByRole('button', { name: 'R3C Launch', exact: true }); await name.click();
      await page.getByRole('heading', { name: 'R3C Launch', exact: true }).waitFor();
      await page.getByRole('tab', { name: 'Overview', exact: true }).waitFor();
      const row = name.locator('..').locator('..');
      await row.getByRole('button', { name: 'Collapse project R3C Launch', exact: true }).click();
      assert.equal(await row.locator('.project-navigation__room').count(), 0);
      await row.getByRole('button', { name: 'Expand project R3C Launch', exact: true }).click();
      await name.click(); await shot('R2-13-project-home');
      await page.getByRole('button', { name: 'general', exact: true }).click();
      await page.getByRole('main', { name: 'Conversation R3C Launch / general', exact: true }).waitFor();
      await page.getByRole('textbox', { name: 'Message #general', exact: true }).waitFor();
      const selected = row.locator('.project-navigation__section:has([aria-current=page])');
      const fills = await selected.evaluate(el => [getComputedStyle(el).backgroundColor, getComputedStyle(el.querySelector('.project-navigation__room')).backgroundColor, getComputedStyle(el.querySelector('.k-icon-btn')).backgroundColor]);
      assert.notEqual(fills[0], 'rgba(0, 0, 0, 0)'); assert.deepEqual(fills.slice(1), ['rgba(0, 0, 0, 0)', 'rgba(0, 0, 0, 0)']);
      await shot('R2-21-22-room-title-selection');
    });
    await check('R3C stopped room has no partial label or untouched stopped speaker', async () => {
      const box = page.getByRole('textbox', { name: 'Message #general', exact: true });
      await box.fill('R3C room stop'); await box.press('Enter');
      await page.getByLabel('Section conversation', { exact: true }).getByText('R3C Iris', { exact: true }).first().waitFor();
      await page.locator('.room-agent .message-body').first().waitFor();
      await page.locator('.composer').getByRole('button', { name: 'Stop', exact: true }).click();
      await page.waitForFunction(() => !document.querySelector('.composer [aria-label=Stop]'));
      await page.waitForFunction(() => !document.querySelector('[aria-label="Reply from R3C Mara"]'));
      assert.equal(await page.getByText('Partial reply', { exact: true }).count(), 0);
      assert.equal(await page.getByRole('region', { name: 'Reply from R3C Mara', exact: true }).count(), 0);
      await shot('R2-23-room-stopped');
    });
    await check('R3C palette fixed top, distinct icons, borderless input and composer focus', async () => {
      await openAgent();
      assert.equal(await page.getByText('No role yet', { exact: true }).count(), 0);
      await page.keyboard.press('Control+k');
      await switcher.getByRole('option', { name: 'R3C Iris', exact: true }).waitFor();
      assert.equal(await switcher.getByRole('option', { name: 'R3C Iris', exact: true }).locator('.avatar').count(), 1);
      assert.equal(await switcher.getByRole('option', { name: 'New agent', exact: true }).locator('.lucide-plus').count(), 1);
      assert.equal(await switcher.getByRole('option', { name: 'Appearance', exact: true }).locator('.lucide-palette').count(), 1);
      const input = switcher.getByRole('combobox'), top = (await switcher.boundingBox()).y;
      await shot('R2-19-palette'); await input.fill('zzzz-no-results');
      assert(Math.abs((await switcher.boundingBox()).y - top) < 1);
      assert.equal(await input.evaluate(el => getComputedStyle(el).boxShadow), 'none');
      await input.fill('R3C Iris'); await input.press('Enter'); await switcher.waitFor({ state: 'hidden' });
      await page.waitForFunction(() => document.activeElement?.matches('.composer__input'));
    });
    await check('R3C inline output and image metadata, human work titles and fork divider', async () => {
      const output = await makeAgent('R3C Output'); await openAgent(output.name);
      const box = page.getByRole('textbox', { name: `Message ${output.name}`, exact: true });
      await box.fill('R3C tool output'); await box.press('Enter');
      const link = page.locator('.agent-message .markdown p').getByRole('link', { name: 'notes.txt', exact: true }); await link.waitFor();
      assert.equal(await page.locator('.agent-message .attachment').count(), 0);
      await page.getByRole('button', { name: /^Worked/ }).last().click();
      await page.getByRole('list', { name: 'Turn activity', exact: true }).getByRole('button', { name: 'Created notes.txt Completed', exact: true }).waitFor();
      assert.equal(await page.getByText(/Result recorded|create_text_file/).count(), 0);
      assert.equal(await page.locator('.activity-timing').count(), 0);
      const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a1XkAAAAASUVORK5CYII=', 'base64');
      const response = await page.request.post(`${origin}/api/agents/${output.id}/files/${randomUUID()}`, { headers: { Origin: origin, 'Content-Type': 'application/octet-stream', 'X-File-Name': 'polish-image.png' }, data: png }); assert(response.ok(), await response.text());
      await showPanel(); await panel.getByRole('tab', { name: 'Files', exact: true }).click();
      const image = panel.getByRole('button', { name: 'Preview polish-image.png', exact: true }); await image.waitFor();
      assert.match(await image.innerText(), /polish-image\.png/); assert.match(await image.innerText(), /\d+ B/);
      assert((await image.boundingBox()).height <= 44); await shot('R2-25-26-files-worklog');
      await panel.getByRole('tab', { name: 'Chats', exact: true }).click();
      assert.equal(await panel.getByRole('searchbox').count(), 0);
      await page.keyboard.press('Control+f');
      const find = page.getByRole('search', { name: 'Find in conversation', exact: true }); await find.getByRole('searchbox').fill('notes');
      assert.equal(await find.getByRole('button', { name: /Close|Clear/ }).count(), 1);
      assert.equal(await find.getByRole('searchbox').getAttribute('type'), 'text');
      await shot('R2-18-find'); await find.getByRole('button', { name: 'Close search', exact: true }).click();
      await page.getByRole('button', { name: 'Fork from here', exact: true }).last().click();
      const divider = page.getByLabel('Fork origin', { exact: true }); await divider.waitFor();
      assert.equal(await divider.innerText(), 'Forked from Main chat'); assert(await divider.getAttribute('title'));
      await page.locator('.fork-history .agent-message .markdown').waitFor();
      const lefts = await divider.evaluate(el => [el.firstElementChild.getBoundingClientRect().left, document.querySelector('.fork-history .agent-message .markdown').getBoundingClientRect().left]);
      assert(Math.abs(lefts[0] - lefts[1]) <= 1, JSON.stringify(lefts)); await shot('R2-27-fork');
      const current = panel.locator('.conversation-row.is-current');
      const fills = await current.evaluate(el => [getComputedStyle(el).backgroundColor, getComputedStyle(el.querySelector('.panel-row')).backgroundColor, getComputedStyle(el.querySelector('.k-icon-btn')).backgroundColor]);
      assert.notEqual(fills[0], 'rgba(0, 0, 0, 0)'); assert.deepEqual(fills.slice(1), ['rgba(0, 0, 0, 0)', 'rgba(0, 0, 0, 0)']);
    });
    await check('R3C narrow lane reserves minimap space and sheet opens without tooltip', async () => {
      await page.setViewportSize({ width: 900, height: 900 });
      const rail = page.locator('.timeline-minimap');
      assert.equal(await rail.isVisible(), false);
      await shot('R2-29-narrow');
      await page.setViewportSize({ width: 700, height: 900 });
      await page.getByRole('button', { name: 'Show details', exact: true }).waitFor(); await showPanel();
      await page.mouse.move(600, 700);
      const sheet = page.getByRole('dialog', { name: 'Conversation panel', exact: true }); await sheet.waitFor();
      await page.waitForFunction(() => document.activeElement?.matches('.right-panel-sheet [role=tab][data-state=active]'));
      assert.equal(await page.getByRole('tooltip').count(), 0); await shot('R2-20-sheet-focus');
      await page.keyboard.press('Escape'); await page.setViewportSize({ width: 1440, height: 900 });
    });
    await check('R3C workflow empty page, model affordance, shortcuts and unavailable Settings', async () => {
      await page.getByRole('button', { name: 'Workflows', exact: true }).click();
      await page.getByRole('heading', { name: 'Workflows', level: 1, exact: true }).waitFor();
      assert.equal(await page.getByRole('button', { name: 'Back', exact: true }).count(), 0);
      assert.equal(await page.getByText('Workflow starting agent', { exact: true }).count(), 0);
      assert.equal(await page.getByRole('main').getByRole('combobox').count(), 0); await shot('R2-14-workflows');
      await page.getByRole('button', { name: 'New workflow', exact: true }).click();
      const create = page.getByRole('dialog', { name: 'New workflow', exact: true }); await create.getByRole('combobox', { name: 'Starting agent', exact: true }).waitFor();
      await page.keyboard.press('Escape');
      await page.getByRole('button', { name: 'Create new', exact: true }).click(); await page.getByRole('menuitem', { name: 'New agent', exact: true }).click();
      const form = page.getByRole('dialog', { name: 'New agent', exact: true });
      assert.equal(await form.getByLabel('Name', { exact: true }).getAttribute('placeholder'), 'Iris');
      await form.getByRole('button', { name: 'Advanced settings', exact: true }).click(); await form.getByRole('button', { name: 'Show model suggestions', exact: true }).click();
      await page.getByRole('listbox').waitFor(); await shot('R2-30-31-new-agent');
      await page.keyboard.press('Escape'); await page.keyboard.press('Escape');
      await page.getByRole('button', { name: 'Help', exact: true }).click();
      await page.getByRole('heading', { name: 'Keyboard shortcuts', exact: true }).waitFor();
      assert(await page.locator('.help-page .k-row__title').evaluateAll(rows => rows.every(row => row.scrollWidth <= row.clientWidth)));
      assert.equal(await page.getByText('How Minarbot works', { exact: true }).count(), 0); await shot('R2-32-help'); await page.keyboard.press('Escape');
      await choose('Settings General'); const settings = page.getByRole('dialog', { name: 'Settings', exact: true });
      await settings.getByRole('heading', { name: 'General', exact: true }).waitFor();
      if (!directory.hubAvailable) assert.equal(await settings.getByRole('button', { name: 'minarbot.com', exact: true }).count(), 0);
      const voices = await page.evaluate(() => typeof speechSynthesis !== 'undefined' ? speechSynthesis.getVoices().filter(v => v.localService).length : 0);
      if (!voices) assert.equal(await settings.getByRole('switch', { name: 'Read aloud', exact: true }).count(), 0);
      await shot('R2-33-general');
      await settings.getByRole('button', { name: 'Usage', exact: true }).click();
      await settings.getByRole('heading', { name: 'Usage', exact: true }).waitFor();
      await settings.getByRole('table').waitFor();
      assert((await settings.locator('.app-settings__page').boundingBox()).width <= 864);
      assert.equal(await settings.getByRole('cell', { name: 'Unknown', exact: true }).count(), 0);
      await shot('R2-34-usage'); await page.keyboard.press('Escape');
    });
  } finally {
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.reload(); await agentChat();
  }
}

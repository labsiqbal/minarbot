import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';

export async function issue1Smoke({ page, check, agentChat, screenshot }) {
  const capture = async name => {
    for (const theme of ['dark', 'light']) {
      await page.evaluate(value => { document.documentElement.dataset.theme = value; }, theme);
      await screenshot(`issue1-${name}-${theme}`);
    }
    await page.evaluate(() => { document.documentElement.dataset.theme = 'dark'; });
  };
  await check('Always-on Computer toggle and keyboard limit explanation', async () => {
    await agentChat();
    const pattern = /\/api\/agents\/[^/]+\/computer$/;
    const status = {
      available: true, enabled: true, alwaysOn: false, alwaysOnLimitReached: true, revision: 7,
      state: 'off', control: { mode: 'agent', revision: 0 }, requests: [],
    };
    const commands = [];
    await page.route(pattern, async route => {
      if (route.request().method() === 'POST') {
        const command = route.request().postDataJSON();
        commands.push(command);
        status.alwaysOn = command.alwaysOn;
        status.revision++;
      }
      await route.fulfill({ json: status });
    });
    try {
      const opener = page.getByRole('button', { name: 'Edit profile', exact: true });
      const dialog = page.getByRole('dialog', { name: 'Agent profile', exact: true });
      const toggle = dialog.getByRole('switch', { name: 'Always-on Computer', exact: true });
      const limit = dialog.getByRole('group', { name: 'Always-on Computer unavailable' });
      await opener.click();
      await dialog.getByRole('tab', { name: 'Advanced', exact: true }).click();
      await toggle.waitFor();
      assert(await toggle.isDisabled());
      await page.keyboard.press('Tab');
      await limit.focus();
      await page.getByRole('tooltip').filter({ hasText: 'Computer limit reached' }).waitFor();
      await capture('computer-limit');
      await page.keyboard.press('Escape');
      status.alwaysOnLimitReached = false;
      await page.waitForFunction(() => !document.querySelector('[role="switch"][aria-label="Always-on Computer"]')?.disabled);
      await toggle.press('Space');
      await dialog.locator('[role="switch"][aria-label="Always-on Computer"][aria-checked="true"]:enabled').waitFor();
      assert.equal(commands.length, 1);
      assert.equal(commands[0].alwaysOn, true);
      assert.equal(commands[0].revision, 7);
      await capture('computer-on');
      await page.keyboard.press('Escape');
      await dialog.waitFor({ state: 'hidden' });
    } finally {
      await page.unroute(pattern);
    }
  });
  await check('Routine templates open and create a GitHub webhook', async () => {
    await page.getByRole('button', { name: 'Routines', exact: true }).click();
    const open = page.getByRole('button', { name: 'New from template', exact: true });
    const picker = page.getByRole('dialog', { name: 'New from template', exact: true });
    await open.click();
    for (const name of ['Slack message', 'Linear issue', 'Sentry alert', 'GitHub issue', 'Schedule']) {
      const option = picker.getByRole('button', { name, exact: true });
      await option.waitFor();
      assert.equal(await option.locator('.k-row__leading svg').count(), 1);
    }
    await capture('routine-templates');
    await page.keyboard.press('Escape');
    await picker.waitFor({ state: 'hidden' });
    await page.waitForFunction(element => element === document.activeElement, await open.elementHandle());
    await open.press('Enter');
    await picker.getByRole('button', { name: 'GitHub issue', exact: true }).press('Enter');
    const form = page.getByRole('dialog', { name: 'New routine', exact: true });
    await form.waitFor();
    assert.equal(await form.getByLabel('Name', { exact: true }).inputValue(), 'GitHub issue');
    assert.equal(await form.getByRole('combobox', { name: 'Trigger', exact: true }).textContent(), 'GitHub issue');
    await form.getByRole('button', { name: 'Save routine', exact: true }).click();
    await page.getByRole('heading', { name: 'GitHub issue webhook', exact: true }).waitFor();
    const url = page.getByLabel('Webhook URL', { exact: true });
    assert.match(await url.inputValue(), /\/api\/webhooks\/[\w-]+$/);
    await page.getByLabel('Signing secret', { exact: true }).fill(randomUUID());
    await page.getByRole('button', { name: 'Enable webhook', exact: true }).click();
    await page.getByRole('button', { name: 'Save webhook', exact: true }).waitFor();
    assert(await page.getByLabel('Signing secret', { exact: true }).inputValue() === '', 'Signing secret field was not cleared');
    await capture('github-webhook');
  });
}

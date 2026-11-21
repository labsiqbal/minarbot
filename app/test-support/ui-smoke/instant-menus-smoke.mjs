import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';

async function sampleMenu(page, button, selector) {
  await page.evaluate(selector => {
    window.menuSamples = new Promise(resolve => {
      document.addEventListener('click', () => {
        const start = performance.now(), samples = [];
        const sample = () => {
          const elapsed = performance.now() - start;
          const menu = document.querySelector(selector);
          samples.push({
            elapsed,
            skeleton: !!menu?.querySelector('.k-skeleton'),
            loading: [...(menu?.querySelectorAll('[role="status"]') ?? [])]
              .map(node => node.textContent).filter(text => /^Loading\b/.test(text)),
          });
          if (elapsed < 500) requestAnimationFrame(sample);
          else resolve(samples);
        };
        requestAnimationFrame(sample);
      }, { capture: true, once: true });
    });
  }, selector);
  await button.click();
  return page.evaluate(() => window.menuSamples);
}

export async function instantMenusSmoke({ page, check, backToChat, origin, agentId }) {
  const delayRead = async route => {
    await delay(650);
    await route.continue();
  };
  const workflowsPath = '**/api/workflows?deleted=0';
  const routinesPath = /\/api\/routines\?search=/;
  const usagePath = '**/api/usage';
  const automation = page.getByRole('navigation', { name: 'Automation' });
  await page.route(workflowsPath, delayRead);
  await page.route(routinesPath, delayRead);
  await page.route(usagePath, delayRead);
  try {
    await check('Instant menus - Workflows chunk prefetch and delayed first skeleton', async () => {
      const prefetched = page.waitForResponse(/\/assets\/WorkflowDirectory-[^/]+\.js$/, { timeout: 2500 }).then(async response => (await response.finished()) === null).catch(() => false);
      await backToChat();
      const idle = await prefetched;
      const workflows = automation.getByRole('button', { name: 'Workflows', exact: true });
      const first = await sampleMenu(page, workflows, '.page-view');
      await page.getByText('No workflows yet', { exact: true }).waitFor();
      await backToChat();
      const second = await sampleMenu(page, workflows, '.page-view');
      const flashes = second.filter(sample => sample.skeleton || sample.loading.length);
      assert.equal(flashes.length, 0, JSON.stringify(flashes[0]));
      assert(idle, 'Workflow chunk should load before the Workflows button is used');
      assert(!first.some(sample => sample.loading.includes('Loading workflow directory...')));
      assert(first.some(sample => sample.elapsed < 200));
      assert(!first.some(sample => sample.elapsed < 200 && sample.skeleton));
      const appeared = first.find(sample => sample.skeleton);
      assert(appeared, 'A slow first read should reveal the skeleton after 200 ms');
      return 'First skeleton at ' + Math.round(appeared.elapsed) + ' ms. Revisit clear across ' + second.length + ' frames.';
    });
    await check('Instant menus - Routines revisit', async () => {
      await backToChat();
      const routines = automation.getByRole('button', { name: 'Routines', exact: true });
      await routines.click();
      await page.getByText('No routines yet', { exact: true }).waitFor();
      await backToChat();
      const frames = await sampleMenu(page, routines, '.page-view');
      assert(!frames.some(frame => frame.skeleton || frame.loading.length), JSON.stringify(frames.filter(frame => frame.skeleton || frame.loading.length)));
      await page.getByText('No routines yet', { exact: true }).waitFor();
      return 'Revisit clear across ' + frames.length + ' frames.';
    });
    await check('Instant menus - Settings Usage revisit', async () => {
      await backToChat();
      await page.getByRole('button', { name: /^Settings(?: - Update available)?$/ }).click();
      const settings = page.getByRole('dialog', { name: 'Settings', exact: true });
      const usage = settings.getByRole('button', { name: 'Usage', exact: true });
      try {
        await usage.click();
        const summary = settings.getByRole('heading', { name: 'Call history', exact: true });
        await summary.waitFor();
        await settings.getByRole('button', { name: 'General', exact: true }).click();
        const frames = await sampleMenu(page, usage, '.app-settings__page');
        assert(!frames.some(frame => frame.skeleton || frame.loading.length), JSON.stringify(frames.filter(frame => frame.skeleton || frame.loading.length)));
        await summary.waitFor();
        return 'Revisit clear across ' + frames.length + ' frames.';
      } finally {
        await settings.getByRole('button', { name: 'Close settings', exact: true }).click();
      }
    });
    await check('Instant menus - routine edit revalidates mounted list', async () => {
      const name = 'Cache revision fixture', id = randomUUID();
      const created = await page.request.post(origin + '/api/agents/' + agentId + '/routines', {
        headers: { Origin: origin },
        data: { id, name, prompt: 'Fixture', trigger: 'manual', nextAt: Date.now() + 3600000, intervalSeconds: null, enabled: false },
      });
      assert(created.ok(), await created.text());
      await automation.getByRole('button', { name: 'Routines', exact: true }).click();
      const toggle = page.getByRole('switch', { name: 'Enable ' + name, exact: true });
      await toggle.waitFor();
      assert.equal(await toggle.getAttribute('aria-checked'), 'false');
      const response = page.waitForResponse(value => value.url().endsWith('/api/routines/' + id + '/edit'));
      const fresh = page.waitForResponse(async value => {
        if (!routinesPath.test(value.url()) || !value.ok()) return false;
        const data = await value.json();
        return data.routines.some(row => row.id === id && !!row.enabled);
      });
      await toggle.click();
      const saved = await response;
      assert(saved.ok(), await saved.text());
      await fresh;
      await toggle.and(page.locator('[aria-checked="true"]')).waitFor();
      return 'Mounted list displays the new revision after edit.';
    });
  } finally {
    await page.unroute(workflowsPath, delayRead);
    await page.unroute(routinesPath, delayRead);
    await page.unroute(usagePath, delayRead);
  }
}

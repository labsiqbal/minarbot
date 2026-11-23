import assert from 'node:assert/strict';
import { assertP95 } from './timing-budgets.mjs';
import { fixDate, parityShot, withFixtureText } from './parity-shot.mjs';
import { randomUUID } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { chromium } from 'playwright-core';
import { startApp } from '../../test-bridge.ts';

const output = resolve(process.argv[2] ?? 'test-support/ui-smoke/artifacts/projects');
await mkdir(output, { recursive: true });
const directory = await mkdtemp(join(tmpdir(), 'minarbot-project-smoke-'));
const workspaces = await mkdtemp(resolve('node_modules/.project-smoke-'));
const folder = join(workspaces, 'git');
// Only rendered fixture paths use this spelling from the golden capture.
const displayedWorkspace = '/home/iqbal/.t3/worktrees/minarbot/t3-f37d9395-993c23ddd0f8f9df/app/node_modules/.project-smoke-fwL9iJ';
const project = '10000000-0000-4000-8000-000000000081';
const agent = '20000000-0000-4000-8000-000000000081';
const connection = '30000000-0000-4000-8000-000000000081';
const base = '/api/projects/' + project;
const url = '/projects/' + project;
const secret = 'fixture-only-environment-secret';
const now = Date.UTC(2026, 0, 1);
const variants = [['desktop', 1440, 900], ['narrow', 390, 844]];
const screens = [
  ['Overview', '', 'Members'],
  ['Connected apps', 'apps', 'Connect apps'],
  ['Environment', 'environment', 'No environment variables'],
  ['MCP', 'mcp', 'No connections'],
];
const receipt = {};
const samples = Number(process.env.MINARBOT_PROJECT_BENCH ?? 20);
assert(Number.isInteger(samples) && samples >= 0);
let browser;

function git(...args) {
  execFileSync('git', ['-C', folder, ...args], { stdio: 'pipe' });
}

function timing(samples) {
  const ordered = samples.toSorted((a, b) => a - b);
  const percentile = value => +ordered[Math.ceil(ordered.length * value) - 1].toFixed(1);
  return { median: percentile(0.5), p95: percentile(0.95), n: samples.length };
}

async function run(client) {
  const app = await startApp({ database: join(directory, client + '.sqlite'), dist: resolve('dist'), port: 0,
    projectRoot: join(workspaces, 'projects'), fixedClock: now, defaultModel: 'fixture', baseURL: 'http://127.0.0.1:1/v1' });
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, timezoneId: 'UTC' });
  const page = await context.newPage();
  await fixDate(page, now);
  const errors = [], csp = [], screenshots = [], mutations = [];
  const mcp = { id: connection, name: 'Fixture MCP', url: 'https://example.test/mcp', revision: 1, enabled: 1, hasToken: true };
  const tools = [{ name: 'echo', description: 'Fixture tool', inputSchema: { type: 'object' } }];
  let installed = false, permitted = [];
  let configured = false;
  await page.route('**/api/**', async route => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (request.method() === 'GET') {
      const rows = {
        [base + '/mcp']: { items: installed ? [mcp] : [], next_after: null },
        [base + '/mcp/' + connection]: { ...mcp, tools, grant: { revision: 0, tools: permitted } },
        [base + '/connected-apps']: { configured, items: configured ? [{ slug: 'github', name: 'GitHub', managedAuth: true }] : [], categories: [], next_after: null },
        [base + '/connected-apps/accounts']: { items: configured ? [{ id: 'fixture-account', toolkit: 'github', status: 'ACTIVE' }] : [] },
        [base + '/agents/' + agent + '/connected-apps']: { items: [] },
        [base + '/agents/' + agent + '/connected-app-policies']: { items: [] },
      };
      return Object.hasOwn(rows, path) ? route.fulfill({ json: rows[path] }) : route.continue();
    }
    if (!path.startsWith('/api/projects')) return route.continue();
    const { id, token, value, revision, updated, test, ...body } = request.postDataJSON();
    assert.match(id, /^[0-9a-f-]{36}$/i);
    if (value || token) assert((value ?? token) === secret, 'Unexpected synthetic secret.');
    if (revision !== undefined) assert(Number.isInteger(revision));
    if (updated !== undefined) assert(updated === null || Number.isInteger(updated));
    if (test !== undefined) assert.equal(test, connection);
    mutations.push([path.replace(/[0-9a-f-]{36}/gi, ':id'), body]);
    switch (path) {
      case base + '/mcp/test':
        return route.fulfill({ json: { id: connection, expires: now + 600_000, tools } });
      case base + '/mcp':
        installed = true;
        return route.fulfill({ json: mcp });
      case base + '/agents/' + agent + '/mcp/' + connection:
        permitted = body.tools;
        return route.fulfill({ json: { revision: 1, tools: permitted } });
      case base + '/mcp/' + connection + '/remove':
        installed = false;
        return route.fulfill({ json: { deleted: true } });
      case base + '/connected-apps/grant-all/github':
        return route.fulfill({ json: { granted: true, members: 1 } });
      default:
        return route.continue();
    }
  });
  page.on('pageerror', error => errors.push(error.message));
  page.on('console', message => {
    if (message.type() === 'error') (message.text().startsWith('CSP ') ? csp : errors).push(message.text());
  });
  await context.addInitScript(() => addEventListener('securitypolicyviolation', event => console.error('CSP ' + event.effectiveDirective)));
  const go = (tab = '', path = url) => {
    const params = new URLSearchParams();
    if (tab) params.set('projectTab', tab);
    return app.origin + path + (params.size ? '?' + params : '');
  };
  const button = (name, scope = page) => scope.getByRole('button', { name, exact: true });
  const text = name => page.getByText(name, { exact: true }).first();
  const selectTab = name => page.getByRole('tablist', { name: 'Project sections' }).getByRole('tab', { name, exact: true }).click();
  const dialog = name => page.getByRole('dialog', { name, exact: true });
  const menu = async (name, item) => {
    await button(name).click();
    await page.getByRole('menuitem', { name: item, exact: true }).click();
  };
  const confirm = name => button(name, page.getByRole('alertdialog')).click();
  const api = async (path, data) => {
    const response = await fetch(app.origin + path, data ? {
      method: 'POST', headers: { Origin: app.origin, 'Content-Type': 'application/json' }, body: JSON.stringify(data),
    } : {});
    assert(response.ok, path + ' returned ' + response.status);
    return response.json();
  };
  const capture = async (name, reopen) => {
    for (const [layout, width, height] of variants) {
      if (reopen && layout === 'narrow') await page.keyboard.press('Escape');
      await page.setViewportSize({ width, height });
      if (reopen) {
        await page.waitForFunction(width => innerWidth === width, width);
        await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
        await reopen();
      }
      const activeTab = page.locator('.project-home > .k-tabs > [role=tablist] > [aria-selected=true]');
      if (await activeTab.isVisible()) await activeTab.scrollIntoViewIfNeeded();
      await page.mouse.move(width - 1, 0);
      for (const theme of ['dark', 'light']) {
        await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
        const file = name + '-' + layout + '-' + theme + (['project-messaging', 'project-whatsapp', 'restore', 'section-restore'].includes(name) ? '-diagnostic' : '') + '.png';
        assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), file + ' overflow');
        await withFixtureText(page, [[workspaces, displayedWorkspace]], () => parityShot(page, join(output, client + '-' + file)));
        screenshots.push(file);
      }
    }
    await page.setViewportSize({ width: 1440, height: 900 });
    if (reopen) {
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      await reopen();
    }
  };
  try {
    await api('/api/agents', { id: agent, runtime: 'api', name: 'Project member', model: 'fixture', instructions: 'Synthetic fixture.' });
    const created = await api('/api/projects', { id: project, name: 'Fixture project', description: 'Project parity fixture.',
      members: [{ agent, write: false }], folders: [{ mode: 'existing', path: folder }], confirmWrite: false });
    // Keep background indexing from racing the screenshots.
    await api(base + '/edit', { id: randomUUID(), revision: created.revision, codegraphEnabled: false });
    await page.goto(go());
    assert.equal(await page.locator('#leptos-root').count(), 1);
    await text('1 changed').waitFor();
    await capture('overview');
    await page.getByRole('button', { name: /notes\.md/ }).click();
    await page.getByRole('region', { name: 'Unified diff' }).getByText('+Changed notes', { exact: true }).waitFor();
    await capture('git-diff');
    await page.keyboard.press('Escape');

    await button('New project', page.getByRole('navigation', { name: 'Projects', exact: true })).press('Enter');
    await dialog('New project').getByLabel('Project name').fill('Created in browser');
    const create = dialog('New project');
    await button('Add folder', create).click();
    const addMember = create.getByRole('combobox', { name: 'Add project member' });
    if (await addMember.evaluate(control => control instanceof HTMLSelectElement)) {
      await addMember.selectOption({ label: 'Project member' });
    } else {
      await addMember.click();
      await page.getByRole('option', { name: 'Project member', exact: true }).click();
    }
    await capture('new-project');
    {
      assert(await create.evaluate(element => element.matches(':modal')), 'New project dialog must open in the top layer');
    }
    await button('Create project', dialog('New project')).click();
    await page.waitForURL(/\/rooms\//);
    const fromBrowser = (await api('/api/projects')).projects.find(project => project.name === 'Created in browser');
    assert.equal(fromBrowser.members[0].agent, agent);
    assert.equal(fromBrowser.folders.length, 1);

    await page.goto(go());
    const openSidebarDialog = async (trigger, item, title) => {
      if (await dialog(title).isVisible()) return;
      const expand = button('Expand sidebar');
      if (await expand.isVisible()) await expand.click();
      const tab = page.getByRole('tab', { name: /^Projects/ }).first();
      if (await tab.getAttribute('aria-selected') !== 'true') await tab.click();
      await menu(trigger, item);
      await dialog(title).waitFor();
    };
    const rename = () => openSidebarDialog('Menu project Fixture project', 'Rename', 'Rename project');
    await capture('rename', rename);
    await rename();
    await dialog('Rename project').getByLabel('Project name').fill('Updated project');
    await button('Save name', dialog('Rename project')).click();
    await page.getByRole('heading', { name: 'Updated project', exact: true }).waitFor();
    const section = () => openSidebarDialog('Menu section general in Updated project', 'Rename section', 'Rename section');
    await capture('section-settings', section);
    await section();
    await dialog('Rename section').getByLabel('Section name').fill('Planning');
    await button('Save name', dialog('Rename section')).click();
    await button('Menu section Planning in Updated project').waitFor({ state: 'attached' });
    await menu('Menu project Updated project', 'New section');
    await dialog('New section').getByLabel('Section name').fill('Review');
    await button('Create section', dialog('New section')).click();
    await menu('Menu section Review in Updated project', 'Move section up');
    await page.waitForFunction(() => Array.from(document.querySelectorAll('.project-navigation__project'))
      .find(node => node.querySelector('.project-navigation__name')?.textContent === 'Updated project')
      ?.querySelector('.project-navigation__room')?.textContent === 'Review');
    await menu('Menu section Review in Updated project', 'Archive section');
    await confirm('Archive section');
    await page.getByRole('navigation', { name: 'Projects', exact: true }).getByText('Review', { exact: true }).waitFor({ state: 'hidden' });

    // Clear the narrow sidebar opened for the rename screenshots.
    await page.setViewportSize({ width: 390, height: 844 });
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    if (await button('Collapse sidebar').isVisible()) await button('Collapse sidebar').click();
    await page.setViewportSize({ width: 1440, height: 900 });

    await selectTab('Connected apps');
    await text('Connect apps').waitFor();
    await capture('apps');
    {
      await button('Project menu').press('Enter');
      await page.getByRole('menuitem', { name: 'Discord and WhatsApp', exact: true }).press('Enter');
      const messaging = dialog('Discord and WhatsApp');
      await button('Connect Discord', messaging).waitFor();
      await capture('project-messaging');
      await text('WhatsApp').click();
      await dialog('WhatsApp').waitFor();
      await capture('project-whatsapp');
      await page.keyboard.press('Escape');
      await button('Close', messaging).click();
    }
    await page.getByRole('switch', { name: 'Allow global apps' }).click();
    await capture('global-apps');
    await confirm('Allow installation apps');
    await page.waitForFunction(() => document.querySelector('[aria-label="Allow global apps"]')?.getAttribute('aria-checked') === 'true');

    configured = true;
    await page.goto(go('apps'));
    await button('Manage').click();
    await button('Allow all members').click();
    await capture('apps-grant');
    await confirm('Allow all members');
    configured = false;
    await page.goto(go('', '/settings/connected-apps'));
    await text('Discord').waitFor();
    await capture('discord');
    await text('WhatsApp').click();
    await dialog('WhatsApp').waitFor();
    await capture('whatsapp');
    await page.goto(go('environment'));

    await text('No environment variables').waitFor();
    await capture('environment');
    await button('Add variable').click();
    const add = dialog('Add variable');
    await add.getByLabel('Variable name').fill('PROJECT_FIXTURE');
    await add.getByLabel('Secret value').fill(secret);
    await capture('environment-editor');
    await button('Add variable', add).click();
    await text('PROJECT_FIXTURE').waitFor();
    assert(!JSON.stringify(await api(base + '/environment')).includes(secret));
    assert(!(await page.locator('body').textContent()).includes(secret));
    await capture('environment-saved');
    await menu('Variable actions for PROJECT_FIXTURE', 'Replace');
    await dialog('Replace PROJECT_FIXTURE').getByLabel('Secret value').fill(secret);
    await button('Replace value', dialog('Replace PROJECT_FIXTURE')).click();
    await confirm('Replace value');
    await dialog('Replace PROJECT_FIXTURE').waitFor({ state: 'hidden' });
    await menu('Variable actions for PROJECT_FIXTURE', 'Delete');
    await confirm('Delete variable');
    await text('No environment variables').waitFor();
    const storage = 'minarbot:project-environment:' + project;
    const pending = { path: base + '/environment', body: { id: randomUUID(), name: 'PROJECT_FIXTURE', updated: null } };
    await page.evaluate(({ storage, pending }) => sessionStorage.setItem(storage, JSON.stringify(pending)), { storage, pending });
    await page.reload();
    await button('Enter value').click();
    await dialog('Add variable').getByLabel('Secret value').fill(secret);
    await dialog('Add variable').locator('button[type=submit]').click();
    await text('PROJECT_FIXTURE').waitFor();
    assert.equal(await page.evaluate(key => sessionStorage.getItem(key), storage), null);
    await menu('Variable actions for PROJECT_FIXTURE', 'Delete');
    await confirm('Delete variable');
    await text('No environment variables').waitFor();

    await selectTab('MCP');
    await text('No connections').waitFor();
    await capture('mcp');
    await button('Add connection').click();
    const editor = dialog('New connection');
    await editor.getByLabel('Name', { exact: true }).fill('Fixture MCP');
    await editor.getByLabel('URL', { exact: true }).fill(mcp.url);
    await editor.getByLabel(/^Token/).fill(secret);
    await capture('mcp-editor');
    await button('Test connection', editor).click();
    await button('Save connection', editor).click();
    await editor.waitFor({ state: 'hidden' });
    await text('Fixture MCP').click();
    await page.getByRole('checkbox', { name: 'Tool: echo', exact: true }).check();
    await capture('mcp-permissions');
    await button('Save permissions').click();
    await confirm('Save permissions');
    await menu('Menu Fixture MCP', 'Remove Fixture MCP');
    await confirm('Remove connection');
    await text('No connections').waitFor();

    await selectTab('Overview');
    await menu('Menu project Updated project', 'Archive');
    await capture('archive');
    await confirm('Archive project');
    await page.waitForURL(location => location.pathname !== url);
    const archived = await api(base);
    assert.equal(archived.archived, true);
    {
      await page.goto(go());
      await page.getByText('Archived projects', { exact: true }).click();
      await menu('Menu project Updated project', 'Restore project');
      await capture('restore');
      await confirm('Restore project');
      await page.getByRole('alertdialog').waitFor({ state: 'hidden' });
      await button('Manage members & folders').waitFor({ state: 'visible' });
      await page.waitForFunction(() => document.querySelector('[aria-label="Manage members & folders"]')?.disabled === false
        || Array.from(document.querySelectorAll('button')).some(button => button.textContent === 'Manage members & folders' && !button.disabled));
    }
    {
      const restored = await api(base);
      assert.equal(restored.archived, false);
      assert.equal(restored.sections.find(section => section.id === created.channel).archived, false);
      assert.equal(restored.sections.find(section => section.name === 'Review').archived, true);
    }
    {
      await menu('Menu section Review in Updated project', 'Restore section');
      await capture('section-restore');
      await confirm('Restore section');
      await page.getByRole('navigation', { name: 'Projects', exact: true }).getByText('Review', { exact: true }).waitFor();
      assert.equal((await api(base)).sections.find(section => section.name === 'Review').archived, false);
    }

    const metrics = {};
    if (samples) {
      for (const [label, tab, ready] of screens) {
        const target = ready;
        const cold = [], warm = [];
        for (let index = 0; index < samples; index++) {
          const fresh = await browser.newContext();
          try {
            const visit = await fresh.newPage();
            const start = performance.now();
            await visit.goto(go(tab), { waitUntil: 'commit' });
            await visit.locator('.project-home h1:visible, .project-home h2:visible, .project-home .k-empty__title:visible')
              .filter({ hasText: target }).first().waitFor();
            cold.push(performance.now() - start);
          } finally { await fresh.close(); }
        }
        await page.goto(go());
        await text('Updated project').waitFor();
        for (let index = 0; index < samples; index++) {
          await selectTab(tab ? 'Overview' : 'Environment');
          warm.push(await page.evaluate(async ({ label, target }) => {
            const control = [...document.querySelectorAll('.project-home [role=tab]')].find(tab => tab.textContent === label);
            const start = performance.now();
            control.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, button: 0 }));
            control.click();
            while (![...document.querySelectorAll('.project-home h1,.project-home h2,.project-home .k-empty__title')]
              .some(node => node.textContent === target && node.getClientRects().length)) {
              if (performance.now() - start > 5000) throw new Error(label + ' tab readiness timed out');
              await new Promise(requestAnimationFrame);
            }
            return performance.now() - start;
          }, { label, target }));
        }
        metrics[label] = { cold: timing(cold), warm: timing(warm) };
        console.log(client, label, JSON.stringify(metrics[label]));
      }
    }
    assert.deepEqual(errors, []);
    assert.deepEqual(csp, []);
    assert.deepEqual(mutations.filter(([path, body]) => !path.endsWith('/restore') && body.archived !== false), [
      ["/api/projects", {"name": "Created in browser", "description": "", "folders": [{"mode": "new"}], "members": [{"agent": "20000000-0000-4000-8000-000000000081", "write": false}], "confirmWrite": false}],
      ["/api/projects/:id/edit", {"name": "Updated project"}],
      ["/api/projects/:id/sections/:id/edit", {"name": "Planning"}],
      ["/api/projects/:id/sections", {"name": "Review"}],
      ["/api/projects/:id/sections/:id/edit", {"position": 0}],
      ["/api/projects/:id/sections/:id/edit", {"archived": true}],
      ["/api/projects/:id/edit", {"allowInstallationApps": true, "confirmed": true}],
      ["/api/projects/:id/connected-apps/grant-all/github", {"confirmed": true}],
      ["/api/projects/:id/environment", {"name": "PROJECT_FIXTURE"}],
      ["/api/projects/:id/environment", {"name": "PROJECT_FIXTURE"}],
      ["/api/projects/:id/environment/remove", {"name": "PROJECT_FIXTURE", "confirmed": true}],
      ["/api/projects/:id/environment", {"name": "PROJECT_FIXTURE"}],
      ["/api/projects/:id/environment/remove", {"name": "PROJECT_FIXTURE", "confirmed": true}],
      ["/api/projects/:id/mcp/test", {"name": "Fixture MCP", "url": "https://example.test/mcp"}],
      ["/api/projects/:id/mcp", {}],
      ["/api/projects/:id/agents/:id/mcp/:id", {"connectionRevision": 1, "tools": ["echo"]}],
      ["/api/projects/:id/mcp/:id/remove", {"confirmed": true}],
      ["/api/projects/:id/archive", {"confirmed": true}],
    ]);
    receipt[client] = { metrics, screenshots, mutations, consoleErrors: errors.length, cspViolations: csp.length };
    for (const [name, timing] of Object.entries(metrics)) {
      assertP95('projects ' + name, 'cold', timing.cold.p95);
      assertP95('projects ' + name, 'warm', timing.warm.p95);
    }
    console.log('PASS', client, screenshots.length, 'screenshots, zero console/CSP errors');
  } catch (error) {
    await page.screenshot({ path: join(output, client + '-failure.png'), animations: 'disabled', mask: [page.locator('input[type=password]')] });
    throw error;
  } finally {
    await context.close();
    await app.close();
  }
}

try {
  await mkdir(folder);
  git('init', '-b', 'main');
  await writeFile(join(folder, 'notes.md'), 'Initial notes\n');
  git('add', 'notes.md');
  git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.test', '-c', 'commit.gpgSign=false', 'commit', '-m', 'Initial notes');
  await writeFile(join(folder, 'notes.md'), 'Changed notes\n');
  browser = await chromium.launch({ args: ['--no-sandbox'] });
  const clients = ['leptos'];
  for (const client of clients) {
    await mkdir(join(workspaces, 'projects'));
    try { await run(client); } finally { await rm(join(workspaces, 'projects'), { recursive: true }); }
  }
} finally {
  await writeFile(join(output, 'receipt.json'), JSON.stringify(receipt, null, 2));
  await browser?.close();
  await rm(directory, { recursive: true });
  await rm(workspaces, { recursive: true });
}

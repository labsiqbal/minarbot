import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';

export async function roomControlsSmoke({ check, page, origin, screenshot, visible, hidden, agentChat, release, leptos = false }) {
  const post = async (path, data) => {
    const response = await page.request.post(origin + path, { headers: { Origin: origin }, data });
    assert(response.ok(), await response.text()); return response.json();
  };
  await check('HRC keyboard addressed Steer and whole-discussion Stop with waiting approval', async () => {
    const members = [];
    for (const [name, model] of [['HRC Alpha', 'hrc-a'], ['HRC Beta', 'hrc-b'], ['HRC Coder', 'hrc-c']]) members.push(await post('/api/agents', {runtime: 'api',  id: randomUUID(), name, model, instructions: 'Synthetic room control fixture.', providerAccount: 'st1-stream' }));
    const project = await post('/api/projects', { id: randomUUID(), name: 'HRC Controls', folders: [{ mode: 'new' }], members: members.map(member => ({ agent: member.id, write: true })), confirmWrite: true });
    await post(`/api/agents/${members[2].id}/edit`, { id: randomUUID(), revision: 1, name: members[2].name, model: 'hrc-c', instructions: 'Synthetic room control fixture.', runtime: 'codex', confirmRuntime: true });
    const path = `/api/channels/${project.sections[0].id}/turns`, root = randomUUID();
    await post(path, { id: root, text: '@HRC Alpha @HRC Coder HRC APPROVE' });
    await page.reload(); await page.getByRole('tab', { name: /^Projects/ }).press('Enter');
    await page.getByRole(leptos ? 'link' : 'button', { name: /^# general in HRC Controls/ }).press('Enter');
    await visible(page.getByRole('main', { name: 'Conversation HRC Controls / general', exact: true }));
    const approval = page.getByRole('region', { name: 'Coding harness approval', exact: true });
    await visible(page.getByText('Room control preview.', { exact: true }));
    const target = page.getByRole('combobox', { name: 'Addressed member', exact: true });
    await target.focus(); await page.keyboard.press('Enter');
    const options = await page.getByRole('option').allTextContents(), index = options.indexOf('HRC Alpha'); assert(index > 0, JSON.stringify(options));
    await page.keyboard.press('Home'); await page.waitForFunction(text => document.activeElement?.textContent === text, options[0]);
    for (let n = 1; n <= index; n++) { await page.keyboard.press('ArrowDown'); await page.waitForFunction(text => document.activeElement?.textContent === text, options[n]); }
    await page.keyboard.press('Enter'); await hidden(page.getByRole('listbox'));
    await page.waitForFunction(() => document.querySelector('[aria-label="Addressed member"]')?.textContent === 'HRC Alpha');
    assert.equal(await page.getByRole('combobox', { name: 'Message mode', exact: true }).count(), 0);
    assert.equal(await target.evaluate(element => !!element.closest('.composer__toolbar')), true);
    const box = page.getByRole('textbox', { name: 'Message #general', exact: true });
    await box.focus(); await page.keyboard.type('HRC addressed guidance');
    const submitted = page.waitForResponse(response => response.url() === origin + path && response.request().method() === 'POST');
    await page.keyboard.press('Control+Enter'); assert((await submitted).ok());
    const rows = await page.request.get(origin + path).then(response => response.json());
    const guidance = rows.find(row => row.text === 'HRC addressed guidance'); assert.equal(guidance.dispatch_mode, 'steer'); assert.equal(guidance.recipient, members[0].id);
    release('hrc-a');
    await page.waitForFunction(async ({ path, id }) => (await fetch(path).then(response => response.json())).find(row => row.id === id)?.status === 'done', { path, id: guidance.id });
    await visible(approval.getByText('Approval needed', { exact: true }));
    await visible(approval.getByRole('button', { name: 'Allow once', exact: true }));
    const capture = async label => {
      for (const width of [1440, 1024]) {
        await page.setViewportSize({ width, height: width === 1440 ? 900 : 768 });
        await screenshot(`hrc-${label}-${width}`);
        assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1 && [...document.querySelectorAll('.room-agent')].every(row => row.scrollWidth <= row.clientWidth + 1)), 'HRC room overflow');
      }
    };
    await capture('addressed-steer-approval');
    const stop = page.locator('.composer-area').getByRole('button', { name: 'Stop', exact: true }); await stop.focus(); await page.keyboard.press('Enter');
    assert.equal(await page.getByRole('alertdialog').count(), 0);
    await page.waitForFunction(async ({ path, root }) => { const rows = await fetch(path).then(response => response.json()); return rows.find(row => row.id === root)?.room.stopped && rows.filter(row => row.room?.root === root).every(row => !['running','queued'].includes(row.status)); }, { path, root });
    await visible(approval.getByText('Expired; ask again', { exact: true }));
    await hidden(approval.getByRole('button', { name: 'Allow once', exact: true }));
    await page.waitForFunction(() => document.activeElement?.tagName === 'TEXTAREA');
    await capture('stopped-approval');
    release('hrc-b');
    await page.setViewportSize({ width: 1440, height: 900 }); await agentChat();
  });
}

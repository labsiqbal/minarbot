import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';

export async function streamActionsSmoke({ check, page, origin, screenshot, releaseStream, releaseRoom, releaseRoomDelivery, streamReply, agentChat, visible, hidden, leptos = false }) {
  const post = async (path, data) => {
    const response = await page.request.post(origin + path, { headers: { Origin: origin }, data });
    assert(response.ok(), await response.text()); return response.json();
  };
  const create = async name => post('/api/agents', {runtime: 'api',  id: randomUUID(), name, instructions: 'Synthetic streaming fixture.', model: 'smoke-primary', backupModel: 'smoke-secondary', providerAccount: 'st1-stream' });
  const openAgent = async name => {
    await page.reload(); await page.getByRole('tab', { name: /^Agents/ }).click();
    await page.getByRole('navigation', { name: 'Choose agent', exact: true }).getByRole(leptos ? 'link' : 'button', { name: new RegExp(`\\b${name}\\b`) }).click();
  };
  const palettes = [['minarbot', 'dark'], ['minarbot', 'light'], ['catppuccin', 'dark']];
  const capture = async label => {
    for (const viewport of [{ width: 1440, height: 900 }, { width: 1024, height: 768 }]) {
      await page.setViewportSize(viewport);
      for (const [palette, theme] of palettes) {
        await page.evaluate(({ palette, theme }) => { document.documentElement.dataset.colorTheme = palette; document.documentElement.dataset.theme = theme; }, { palette, theme });
        await screenshot(`st1-${label}-${palette}-${theme}-${viewport.width}`);
        assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), 'Page overflow');
      }
    }
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.evaluate(() => { document.documentElement.dataset.colorTheme = 'minarbot'; document.documentElement.dataset.theme = 'dark'; });
  };
  let agent, turn;
  await check('ST1 progressive reply, stable blocks, work log, follow and icon actions', async () => {
    agent = await create('ST1 Stream'); await openAgent(agent.name);
    const box = page.getByRole('textbox', { name: `Message ${agent.name}`, exact: true });
    await box.fill('ST1 progressive stream'); await box.press('Enter');
    turn = page.locator('.turn-block').filter({ has: page.getByText('ST1 progressive stream', { exact: true }) });
    await visible(turn.getByLabel('Streaming answer', { exact: true }));
    await visible(turn.getByText('Growing second block', { exact: true }));
    await turn.locator('.stream-markdown .markdown').first().evaluate(element => { window.__st1Block = element; window.__st1Paragraph = element.firstElementChild; window.__st1Mutations = 0; window.__st1Observer = new MutationObserver(records => { window.__st1Mutations += records.length; }); window.__st1Observer.observe(element, { subtree: true, childList: true, characterData: true, attributes: true }); });
    assert.equal(await turn.locator('.turn-activity .k-collapsible__trigger').count(), 0);
    const workingBefore = await turn.locator('.activity-label').textContent();
    await page.waitForFunction(({ id, before }) => document.querySelector(`#${id} .activity-label`)?.textContent !== before, { id: await turn.getAttribute('id'), before: workingBefore });
    assert.notEqual(await turn.locator('.activity-label').textContent(), workingBefore, 'Elapsed time did not tick');
    await capture('midstream');
    const transcript = page.getByRole('region', { name: 'Conversation history', exact: true });
    await transcript.evaluate(element => { element.scrollTop = element.scrollHeight; element.dispatchEvent(new Event('scroll')); });
    releaseStream();
    await page.waitForFunction(() => document.querySelector('.transcript')?.textContent.includes('Paragraph 20'));
    await page.waitForFunction(() => { const el = document.querySelector('.transcript'); return el.scrollHeight - el.scrollTop - el.clientHeight < 3; });
    await page.waitForFunction(() => { const el = document.querySelector('.transcript'); return el.scrollHeight - el.clientHeight > 350; });
    await transcript.evaluate(element => { element.scrollTop -= 250; element.dispatchEvent(new Event('scroll')); });
    await visible(page.getByRole('button', { name: 'Jump to latest', exact: true }));
    const top = await transcript.evaluate(element => element.scrollTop);
    await page.waitForTimeout(300);
    assert(Math.abs(await transcript.evaluate(element => element.scrollTop) - top) <= 1, 'Streaming yanked reader to bottom');
    await page.getByRole('button', { name: 'Jump to latest', exact: true }).click();
    await hidden(page.getByRole('button', { name: 'Jump to latest', exact: true }));
    await visible(turn.getByRole('button', { name: 'Copy reply', exact: true }));
    assert(await page.evaluate(() => window.__st1Block?.isConnected && window.__st1Paragraph === window.__st1Block.firstElementChild), 'Earlier block remounted');
    assert.equal(await page.evaluate(() => window.__st1Mutations), 0, 'Earlier block re-rendered');
    await page.evaluate(() => window.__st1Observer.disconnect());
    assert.equal(await turn.locator('.turn-status, .turn-model').count(), 0);
    assert.equal(await turn.locator('.turn-activity .k-collapsible__trigger').count(), 0);
    assert.match(await turn.locator('.activity-label').textContent(), /Worked for/);
    const stored = await page.request.get(origin + `/api/agents/${agent.id}/turns`).then(res => res.json());
    assert.equal(stored[0].reply, streamReply);
    const copy = turn.getByRole('button', { name: 'Copy reply', exact: true });
    await page.evaluate(() => { Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: async text => { window.__st1Copied = text; } } }); });
    await copy.hover(); await visible(page.getByRole('tooltip', { name: 'Copy reply', exact: true })); await copy.click();
    assert.equal(await page.evaluate(() => window.__st1Copied), stored[0].reply);
    await visible(turn.getByRole('status').filter({ hasText: 'Copied' }));
    await page.evaluate(() => { window.__st1Exec = document.execCommand; document.execCommand = () => false; navigator.clipboard.writeText = async () => { throw new Error('Synthetic clipboard denial'); }; });
    await copy.click(); await visible(turn.getByRole('alert').filter({ hasText: 'Text could not be copied.' }));
    await page.evaluate(() => { document.execCommand = window.__st1Exec; delete window.__st1Exec; navigator.clipboard.writeText = async text => { window.__st1Copied = text; }; });
    await copy.click();

    const fork = turn.getByRole('button', { name: 'Fork from here', exact: true }); await fork.hover(); await visible(page.getByRole('tooltip', { name: 'Fork from here', exact: true }));
    await fork.click();
    const forkOrigin = page.getByLabel('Fork origin', { exact: true }); await visible(forkOrigin);
    await visible(page.getByRole('region', { name: 'Inherited messages', exact: true }));
    await forkOrigin.getByRole('link', { name: 'Main chat', exact: true }).click(); await hidden(forkOrigin);
    await page.getByRole('button', { name: 'Back to latest messages', exact: true }).click();
    for (const palette of ['minarbot', 'github', 'catppuccin', 'everforest', 'solarized', 'midnight']) for (const theme of ['dark', 'light']) {
      await page.evaluate(({ palette, theme }) => { document.documentElement.dataset.colorTheme = palette; document.documentElement.dataset.theme = theme; }, { palette, theme });
      assert.equal(await copy.evaluate(element => getComputedStyle(element).visibility), 'visible');
      assert(await turn.evaluate(element => element.scrollWidth <= element.clientWidth + 1), `${palette}/${theme} transcript overflow`);
    }
    // Reproduce a delayed programmatic scroll event after content grows, before ResizeObserver runs.
    await transcript.evaluate(element => {
      const growth = document.createElement('div'); growth.dataset.followProbe = ''; growth.style.height = '128px';
      element.firstElementChild.append(growth);
      element.dispatchEvent(new Event('scroll'));
    });
    try { await page.waitForFunction(() => { const el = document.querySelector('.transcript'); return el.scrollHeight - el.scrollTop - el.clientHeight < 3; }); }
    finally { await transcript.locator('[data-follow-probe]').evaluate(element => element.remove()); }
    await capture('finished');
  });
  await check('ST1 failed and fallback status stay visible; action focus and hover reveal', async () => {
    const path = `/api/agents/${agent.id}/turns`;
    await post(path, { id: randomUUID(), text: 'ST1 failed reply' });
    const failed = page.locator('.turn-block').filter({ has: page.getByText('ST1 failed reply', { exact: true }) });
    await visible(failed.getByRole('button', { name: 'Retry', exact: true }));
    assert.equal(await failed.locator('.turn-status').count(), 0);
    await failed.getByRole('button', { name: 'Retry', exact: true }).click();
    const resume = page.getByRole('dialog', { name: 'Resume or reassign', exact: true }); await visible(resume);
    assert(await resume.getByRole('button', { name: 'Resume task', exact: true }).isDisabled()); await page.keyboard.press('Escape');
    await post(path, { id: randomUUID(), text: 'ST1 fallback reply' });
    const fallback = page.locator('.turn-block').filter({ has: page.getByText('ST1 fallback reply', { exact: true }) });
    await visible(fallback.locator('.turn-status').getByText('Done · Fallback model used', { exact: true }));
    await fallback.locator('.turn-activity .k-collapsible__trigger').click(); await visible(fallback.getByText('Fallback model used: smoke-secondary', { exact: true }));
    await page.getByRole('textbox', { name: `Message ${agent.name}`, exact: true }).focus();
    assert.equal(await turn.locator('.turn-controls').evaluate(element => getComputedStyle(element).opacity), '0');
    await turn.getByRole('button', { name: 'Copy reply', exact: true }).focus();
    await page.waitForFunction(() => getComputedStyle(document.activeElement.closest('.turn-controls')).opacity === '1');
    await turn.hover(); assert.equal(await turn.locator('.turn-controls').evaluate(element => getComputedStyle(element).opacity), '1');
  });
  await check('ST1 three room agents keep single-line headers and stream in place at both widths', async () => {
    const agents = [agent, await create('ST1 Helper'), await create('ST1 Reviewer')];
    const project = await post('/api/projects', { id: randomUUID(), name: 'ST1 Room', description: 'Synthetic room', members: agents.map(agent => ({ agent: agent.id, write: false })) });
    const projectList = await page.request.get(origin + '/api/projects').then(res => res.json());
    const section = projectList.projects.find(item => item.id === project.id).sections[0];
    await page.reload(); await page.getByRole('tab', { name: /^Projects/ }).click();
    await page.getByRole(leptos ? 'link' : 'button', { name: /^# general in ST1 Room/ }).click();
    const box = page.getByRole('textbox', { name: 'Message #general', exact: true });
    await box.fill('ST1 room stream'); await box.press('Enter');
    const roomTurn = page.locator('.turn-block').filter({ has: page.getByText('ST1 room stream', { exact: true }) });
    const rows = roomTurn.locator('.room-pending'); await page.waitForFunction(() => document.querySelectorAll('.room-pending').length === 3);
    await page.waitForFunction(() => document.querySelectorAll('[aria-label^="Streaming answer from"]').length === 1);
    const closePanel = page.getByRole('button', { name: 'Close details', exact: true });
    if (await closePanel.isVisible()) await closePanel.click();
    for (const viewport of [{ width: 1440, height: 900 }, { width: 1024, height: 768 }]) {
      await page.setViewportSize(viewport);
      for (const row of await rows.all()) {
        const layout = await row.evaluate(element => {
          const rect = element.getBoundingClientRect(), labels = [...element.querySelectorAll('.room-agent__name, .room-agent__status')];
          return { height: rect.height, width: rect.width, scroll: element.scrollWidth, labels: labels.map(el => ({ height: el.getBoundingClientRect().height, lineHeight: parseFloat(getComputedStyle(el).lineHeight), whiteSpace: getComputedStyle(el).whiteSpace, scroll: el.scrollWidth, width: el.clientWidth, text: el.textContent })) };
        });
        assert(layout.height <= 34, JSON.stringify(layout)); assert(layout.scroll <= layout.width + 1, 'Room header overflow');
        for (const label of layout.labels) { assert(label.height <= label.lineHeight + 1 && label.whiteSpace === 'nowrap', JSON.stringify(label)); if (/Working/.test(label.text)) assert(label.scroll <= label.width + 1, 'Working time clipped'); }
        assert.equal(await row.locator('..').getByRole('group', { name: 'Message actions', exact: true }).getByRole('button', { name: 'Work log actions', exact: true }).count(), 1);
        assert.equal(await row.getByRole('button', { name: 'Stop', exact: true }).count(), 0);
      }
      await screenshot(`st1-room-three-agents-midstream-${viewport.width}`);
    }
    await rows.first().hover();
    await rows.first().locator('..').getByRole('button', { name: 'Work log actions', exact: true }).click();
    await visible(page.getByRole('menuitem', { name: 'Pause', exact: true }));
    await visible(page.getByRole('menuitem', { name: 'Skip', exact: true }));
    await page.keyboard.press('Escape');
    await post(`/api/channels/${section.id}/turns`, { id: randomUUID(), text: 'ST1 queued room' });
    const queuedTurn = page.locator('.turn-block').filter({ has: page.getByText('ST1 queued room', { exact: true }) });
    await visible(queuedTurn.getByText('Queued', { exact: true }).first());
    for (const viewport of [{ width: 1440, height: 900 }, { width: 1024, height: 768 }]) {
      await page.setViewportSize(viewport);
      assert.equal(await queuedTurn.locator('.room-pending').count(), 3);
      for (const row of await queuedTurn.locator('.room-pending').all()) assert(await row.evaluate(element => element.getBoundingClientRect().height <= 34 && element.scrollWidth <= element.clientWidth + 1), 'Queued header wrapped');
      await screenshot(`st1-room-three-agents-queued-${viewport.width}`);
    }
    releaseRoom(); await page.waitForFunction(() => !document.querySelector('.room-pending'));
    assert.equal(await roomTurn.locator('.room-agent').count(), 3);
    await capture('room-finished');
    await box.fill('HRA ordered room'); await box.press('Enter');
    const delivery = page.locator('.turn-block').filter({ has: page.getByText('HRA ordered room', { exact: true }) });
    await visible(delivery.getByText('Answer from ST1 Stream: growing', { exact: true }));
    await visible(delivery.getByLabel('Streaming answer from ST1 Stream', { exact: true }));
    assert.equal(await delivery.locator('[aria-label^="Streaming answer from"]').count(), 1);
    assert.equal(await delivery.locator('.room-agent').count(), 3);
    assert.equal(await delivery.getByLabel('Reply from ST1 Reviewer', { exact: true }).locator('.message-body').count(), 0);
    const show = delivery.getByLabel('Reply from ST1 Stream', { exact: true }).locator('.room-agent__status');
    await show.focus(); await page.keyboard.press('Enter');
    await visible(delivery.getByText('Before tool for ST1 Stream.', { exact: true }));
    const prose = delivery.getByText('Before tool for ST1 Stream.', { exact: true });
    const tool = delivery.getByText('Created hra.txt Completed', { exact: true });
    assert(await prose.evaluate((element, other) => !!(element.compareDocumentPosition(other) & Node.DOCUMENT_POSITION_FOLLOWING), await tool.elementHandle()));
    await page.evaluate(() => { const node = document.querySelector('[aria-label="Streaming answer from ST1 Stream"] p'); const range = document.createRange(); range.selectNodeContents(node); getSelection().removeAllRanges(); getSelection().addRange(range); });
    const selection = await page.evaluate(() => getSelection().toString());
    await page.waitForTimeout(450); assert.equal(await page.evaluate(() => getSelection().toString()), selection);
    for (const viewport of [{ width: 1440, height: 900 }, { width: 1024, height: 768 }]) {
      await page.setViewportSize(viewport); await screenshot(`hra-room-serialized-${viewport.width}`);
      assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
    }
    const path = `/api/channels/${section.id}/turns`;
    const beforeReload = await page.request.get(origin + path).then(res => res.json());
    const pending = beforeReload.find(turn => turn.text === 'HRA ordered room');
    const getPartial = rows => rows.filter(turn => turn.room?.root === pending.id).map(turn => turn.partial);
    await page.reload(); await page.getByRole('tab', { name: /^Projects/ }).click();
    await page.getByRole(leptos ? 'link' : 'button', { name: /^# general in ST1 Room/ }).click();
    await visible(delivery.getByText('Answer from ST1 Stream: growing', { exact: true }));
    assert.deepEqual(getPartial(await page.request.get(origin + path).then(res => res.json())), getPartial(beforeReload));
    releaseRoomDelivery();
    await visible(delivery.getByText('Answer from ST1 Stream: growing. Complete.', { exact: true }));
    await visible(delivery.getByText('Answer from ST1 Helper: growing. Complete.', { exact: true }));
    await page.waitForFunction(() => !document.querySelector('[aria-label^="Streaming answer from"]'));
    await page.waitForFunction(() => !document.querySelector('.room-pending'));
    assert.equal(await delivery.locator('.room-agent').count(), 2);
    assert.equal(await delivery.getByText('Before tool for ST1 Stream.', { exact: true }).count(), 0);
    await delivery.getByLabel('Reply from ST1 Stream', { exact: true }).locator('.room-agent__status').focus(); await page.keyboard.press('Enter');
    await visible(delivery.getByText('Before tool for ST1 Stream.', { exact: true }));
    for (const viewport of [{ width: 1440, height: 900 }, { width: 1024, height: 768 }]) {
      await page.setViewportSize(viewport); await screenshot(`hra-room-final-${viewport.width}`);
    }
    await agentChat();
  });
}

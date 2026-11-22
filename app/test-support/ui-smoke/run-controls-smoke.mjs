import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

export async function runControlsSmoke({ check, page, origin, visible, hidden, agentChat, leptos = false, model = 'smoke-primary', providerAccount = 'st1-stream', hold = 'UXA held stream', fail = 'UXA fail' }) {
  const post = async (path, data) => {
    const response = await page.request.post(origin + path, { headers: { Origin: origin }, data });
    assert(response.ok(), await response.text()); return response.json();
  };
  const out = fileURLToPath(new URL('./artifacts/ux-run-controls/after/', import.meta.url));
  await mkdir(out, { recursive: true });
  const shot = async name => page.screenshot({ path: `${out}${name}.png`, animations: 'disabled' });
  let agent = await post('/api/agents', {runtime: 'api',  id: randomUUID(), name: `UXA controls ${randomUUID().slice(0, 6)}`, model, ...(providerAccount ? { providerAccount } : {}), instructions: 'Synthetic only.' });
  let path = `/api/agents/${agent.id}/turns`;
  const read = async () => page.request.get(origin + path).then(response => response.json());
  const open = async () => {
    await page.reload(); await page.getByRole('tab', { name: /^Agents/ }).click();
    await page.getByRole('navigation', { name: 'Choose agent', exact: true }).getByRole(leptos ? 'link' : 'button', { name: new RegExp(`\\b${agent.name}\\b`) }).click();
  };
  await open();
  let box = page.getByRole('textbox', { name: `Message ${agent.name}`, exact: true });
  const stack = page.getByRole('region', { name: 'Queued messages', exact: true });
  let root;
  await check('R3B user bubble and Working row appear within 150ms of Enter', async () => {
    const text = 'R3B immediate send';
    await box.fill(text);
    await page.evaluate(() => {
      window.__r5CLS = 0;
      window.__r5ShiftObserver = new PerformanceObserver(list => { for (const entry of list.getEntries()) if (!entry.hadRecentInput) window.__r5CLS += entry.value; });
      window.__r5ShiftObserver.observe({ type: 'layout-shift' });
    });
    await box.evaluate((element, text) => {
      element.addEventListener('keydown', () => {
        const start = performance.now(); window.__r3bLatency = null;
        const observer = new MutationObserver(() => {
          const row = [...document.querySelectorAll('main article')].find(row => row.querySelector('.human-bubble')?.textContent === text);
          if (row && /Working…/.test(row.textContent)) { window.__r3bLatency = performance.now() - start; observer.disconnect(); }
        }); observer.observe(element.closest('main'), { subtree: true, childList: true, characterData: true });
      }, { once: true, capture: true });
    }, text);
    await box.press('Enter');
    await page.waitForFunction(() => window.__r3bLatency !== null);
    const latency = await page.evaluate(() => window.__r3bLatency);
    assert(latency < 150, `Bubble and Working row took ${latency}ms`);
    await page.waitForFunction(async path => (await fetch(path).then(response => response.json())).at(-1)?.status === 'done', path);
    await hidden(page.locator('.composer').getByRole('button', { name: 'Stop', exact: true }));
    const cls = await page.evaluate(() => { window.__r5ShiftObserver.disconnect(); return window.__r5CLS; });
    assert(cls < 0.05, `Send CLS ${cls} exceeds 0.05`);
  });
  await check('UXA Enter queues outside transcript; inline Edit, Cancel, Remove and Send now work', async () => {
    await box.fill(hold); await box.press('Enter');
    const stop = page.locator('.composer').getByRole('button', { name: 'Stop', exact: true }); await visible(stop);
    root = (await read()).find(row => row.text === hold).id;
    assert.equal(await page.getByRole('combobox', { name: 'Message mode', exact: true }).count(), 0);
    await box.fill('UXA queued first'); await box.press('Enter');
    await box.fill('UXA queued second'); await box.press('Enter');
    await visible(stack.getByText('UXA queued second', { exact: true }));
    assert.equal(await page.locator('.turn-block').filter({ hasText: 'UXA queued first' }).count(), 0);
    await shot('UX-01'); await shot('UX-06');
    const showDetails = page.getByRole('button', { name: 'Show details', exact: true }); if (await showDetails.isVisible()) await showDetails.click();
    await page.keyboard.press('Control+f');
    const search = page.getByRole('searchbox', { name: 'Find in conversation', exact: true });
    await search.fill(hold); await search.press('Enter');
    await visible(page.getByRole('search', { name: 'Find in conversation', exact: true }));
    await visible(stack.getByText('UXA queued second', { exact: true })); await visible(stop);
    await page.getByRole('button', { name: 'Close search', exact: true }).click();
    await visible(stack.getByText('UXA queued second', { exact: true }));
    const first = stack.getByRole('listitem', { name: 'Queued message: UXA queued first', exact: true });
    await first.getByRole('button', { name: 'Edit', exact: true }).click();
    const edit = first.getByRole('textbox', { name: 'Replacement message', exact: true }); await visible(edit); await edit.fill('UXA edited first');
    await edit.press('Shift+Enter'); assert.equal(await edit.inputValue(), 'UXA edited first\n');
    await edit.fill('UXA edited first'); await edit.press('Enter'); await hidden(edit);
    const edited = stack.getByRole('listitem', { name: 'Queued message: UXA edited first', exact: true }); await visible(edited);
    await visible(edited.getByRole('button', { name: 'Send now', exact: true }));
    await edited.getByRole('button', { name: 'Edit', exact: true }).click();
    const inline = edited.getByRole('textbox', { name: 'Replacement message', exact: true });
    await visible(inline); await inline.fill('Canceled draft'); await inline.press('Escape');
    await hidden(inline); assert.equal(await edited.getByText('Discard draft copy', { exact: true }).count(), 0);
    const second = stack.getByRole('listitem', { name: 'Queued message: UXA queued second', exact: true });
    await second.getByRole('button', { name: 'Remove', exact: true }).click(); await hidden(second);
    assert.equal((await read()).find(row => row.text === 'UXA queued second').status, 'cancelled');
    await page.locator(`#turn-${root}`).getByRole('group', { name: 'Message actions', exact: true }).getByRole('button', { name: 'Task actions', exact: true }).click(); await visible(page.getByRole('menuitem', { name: 'Pause', exact: true })); await page.keyboard.press('Escape');
  });
  await check('UXA Stop needs one click, keeps queue, and stale retry cannot stop next turn', async () => {
    await visible(page.locator(`#turn-${root}`).getByLabel('Streaming answer', { exact: true }).getByText('Synthetic active reply.', { exact: true }));
    await box.fill('UXA next turn'); await box.press('Enter'); await visible(stack.getByText('UXA next turn', { exact: true }));
    const cancelPath = `${path}/${root}/cancel`;
    await page.evaluate(path => {
      const original = window.fetch;
      window.fetch = async (...args) => {
        const [input, init] = args;
        const request = input instanceof Request ? input : null;
        if (new URL(request?.url ?? String(input), location.href).pathname === path && (request?.method ?? init?.method) === 'POST') {
          window.fetch = original; window.__uxaCancel = request ? await request.clone().json() : JSON.parse(init.body);
          throw new TypeError('Synthetic Stop transport failure');
        }
        return original(...args);
      };
    }, cancelPath);
    await page.locator('.composer').getByRole('button', { name: 'Stop', exact: true }).click();
    await visible(page.getByRole('button', { name: 'Retry Stop', exact: true }));
    assert.equal(await page.getByRole('alertdialog').count(), 0);
    const captured = await page.evaluate(() => window.__uxaCancel);
    const retried = page.waitForRequest(request => request.url() === origin + cancelPath && request.method() === 'POST');
    await page.getByRole('button', { name: 'Retry Stop', exact: true }).click();
    assert.deepEqual((await retried).postDataJSON(), captured);
    const stopped = page.locator(`#turn-${root}`); await visible(stopped.locator('.activity-label').filter({ hasText: /^Stopped after / }));
    assert.equal(await stopped.locator('.message-error, .message-partial, .turn-status').count(), 0);
    await visible(stopped.getByText('Synthetic active reply.', { exact: true }));
    await shot('UX-05');
    await page.waitForFunction(async ({ path }) => { const rows = await fetch(path).then(res => res.json()); return rows.find(row => row.text === 'UXA next turn')?.status === 'done'; }, { path });
    await open(); await visible(stopped.getByText('Synthetic active reply.', { exact: true }));
    await box.fill(hold); await box.press('Enter'); await visible(page.locator('.composer').getByRole('button', { name: 'Stop', exact: true }));
    const nextRoot = (await read()).at(-1).id;
    await page.waitForFunction(async ({ path, id }) => (await fetch(path).then(res => res.json())).find(row => row.id === id)?.status === 'running', { path, id: nextRoot });
    await visible(page.locator(`.composer-stop[data-turn="${nextRoot}"]`));
    await post(`${path}/${root}/cancel`, captured);
    assert.equal((await read()).find(row => row.id === nextRoot).status, 'running');
    await box.fill('UXA restarted message'); await box.press('Enter');
    const item = stack.getByRole('listitem', { name: 'Queued message: UXA restarted message', exact: true }); await visible(item);
    await item.getByRole('button', { name: 'Send now', exact: true }).click();
    await page.waitForFunction(async ({ path, id }) => (await fetch(path).then(res => res.json())).find(row => row.id === id)?.status === 'cancelled', { path, id: nextRoot });
  });
  await check('UXA F6 reaches composer; each turn has one tab stop and arrow actions', async () => {
    await page.keyboard.press('F6'); assert(await box.evaluate(element => element === document.activeElement));
    const first = page.locator('.turn-block').first(); await first.focus(); await page.keyboard.press('ArrowRight');
    assert(await first.evaluate(element => element.contains(document.activeElement) && element !== document.activeElement));
    assert(await first.evaluate(element => ['BUTTON', 'A', 'INPUT', 'TEXTAREA', 'SELECT'].includes(document.activeElement.tagName)));
    await first.focus(); await page.keyboard.press('ArrowLeft');
    assert(await first.evaluate(element => document.activeElement === [...element.querySelectorAll('button:not(:disabled),a[href],input:not(:disabled),textarea:not(:disabled),select:not(:disabled)')].filter(action => action.getClientRects().length && getComputedStyle(action).visibility !== 'hidden').at(-1)));
    assert.equal(await first.locator('[tabindex="0"]').count(), 0);
    await page.keyboard.press('F6'); await shot('UX-12');
  });
  await check('UXA inline mic timer, Cancel, Stop uploads audio chip without dialog', async () => {
    agent = await post('/api/agents', {runtime: 'api',  id: randomUUID(), name: 'UXA audio', model, ...(providerAccount ? { providerAccount } : {}), instructions: 'Synthetic only.' });
    path = `/api/agents/${agent.id}/turns`; await open();
    box = page.getByRole('textbox', { name: `Message ${agent.name}`, exact: true });
    await page.waitForFunction(() => !document.querySelector('.queued-runs'));
    await page.evaluate(() => {
      window.__uxaStops = 0;
      navigator.mediaDevices.getUserMedia = async () => ({ getTracks: () => [{ stop: () => window.__uxaStops++ }] });
      window.MediaRecorder = class {
        static isTypeSupported() { return true; }
        state = 'inactive'; mimeType = 'audio/webm';
        start() { this.state = 'recording'; }
        stop() { this.state = 'inactive'; queueMicrotask(() => { this.ondataavailable?.({ data: new Blob([new Uint8Array([0x1a, 0x45, 0xdf, 0xa3, 0x87, 0x42, 0x82, 0x84, 0x77, 0x65, 0x62, 0x6d]), 'UXA synthetic audio'], { type: this.mimeType }) }); this.onstop?.(); }); }
      };
    });
    await page.getByRole('button', { name: 'Record audio', exact: true }).click();
    const pill = page.getByRole('group', { name: 'Audio recording', exact: true });
    await visible(pill.getByRole('timer', { name: 'Recording duration', exact: true }));
    assert.equal(await pill.locator('.voice-input__dot').count(), 1);
    assert.equal(await page.getByRole('dialog', { name: 'Record audio', exact: true }).count(), 0);
    await page.waitForFunction(() => document.querySelector('[role="timer"]')?.textContent !== '0:00');
    await shot('UX-03');
    await pill.getByRole('button', { name: 'Cancel recording', exact: true }).click(); await hidden(pill);
    assert(await page.evaluate(() => window.__uxaStops > 0));
    await page.getByRole('button', { name: 'Record audio', exact: true }).click(); await visible(pill);
    await pill.getByRole('button', { name: 'Stop recording', exact: true }).click(); await hidden(pill);
    await visible(page.getByRole('group', { name: 'Composer attachments', exact: true }));
    assert.match(await page.getByRole('group', { name: 'Composer attachments', exact: true }).innerText(), /Voice note \d+:\d{2}/);
    assert(await page.getByRole('button', { name: 'Send message', exact: true }).isEnabled());
    assert(await page.getByRole('group', { name: 'Composer attachments', exact: true }).evaluate(element => !!element.closest('.composer')));
    await shot('UX-03-audio-chip');
    await page.getByRole('button', { name: 'Send message', exact: true }).click();
    await hidden(page.getByRole('group', { name: 'Composer attachments', exact: true }));
    await page.waitForFunction(async path => (await fetch(path).then(response => response.json())).at(-1)?.status === 'done', path);
    await open();
  });
  await check('UXA failures expose Retry and overflow Change model/Dismiss', async () => {
    await box.fill(fail); await box.press('Enter');
    const failed = page.locator('.turn-block').filter({ hasText: fail });
    await visible(failed.getByRole('button', { name: 'Retry', exact: true }));
    assert.equal(await failed.locator('.turn-status').count(), 0);
    await failed.getByRole('button', { name: 'Recovery actions', exact: true }).click();
    await visible(page.getByRole('menuitem', { name: 'Change model', exact: true })); await visible(page.getByRole('menuitem', { name: 'Dismiss', exact: true })); await shot('UX-24');
    await page.getByRole('menuitem', { name: 'Dismiss', exact: true }).click(); await hidden(failed.getByRole('button', { name: 'Retry', exact: true }));
  });
  if (agentChat) await agentChat();
}

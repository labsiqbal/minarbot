import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir } from 'node:fs/promises';
import { join } from 'node:path';

export async function deleteAgentSmoke({ page, check, origin, root }) {
  const evidence = join(root, 'app/test-support/ui-smoke/artifacts/delete-agent');
  await mkdir(evidence, { recursive: true });
  const viewport=page.viewportSize();
  await check('Agent menu supports edit, hide, restore, right-click and permanent deletion', async () => {
    const agent = await page.request.post(origin + '/api/agents', { headers:{Origin:origin},data:{ runtime: 'api',id:randomUUID(),name:'Delete fixture',model:'smoke-primary',instructions:'Help'}});
    assert.equal(agent.status(),201); const {id}=await agent.json();
    await page.reload();await page.setViewportSize({width:1440,height:900});
    await page.getByRole('tab',{name:/^Agents/}).click();
    const row=page.getByRole('button',{name:'Delete fixture Idle',exact:true}), actions=page.getByRole('button',{name:'Actions for Delete fixture',exact:true});
    await row.waitFor();await row.click();await row.hover();await actions.click();
    const menu=page.getByRole('menu');
    assert.deepEqual(await menu.getByRole('menuitem').allTextContents(),['Edit','Inbox','Hide','Delete']);
    assert.equal(await menu.getByRole('separator').count(),1);
    await page.screenshot({path:join(evidence,'menu.png'),animations:'disabled'});
    await menu.getByRole('menuitem',{name:'Hide',exact:true}).click();
    await row.locator('.bot-row__preview').filter({hasText:'Hidden'}).waitFor();
    assert.equal(await row.getAttribute('aria-current'),'page');
    await page.getByRole('button',{name:new RegExp('^Smoke Lead')}).first().click();
    await row.waitFor({state:'hidden'});
    await page.locator('.hidden-agents').getByRole('button',{name:/^Hidden/}).click();
    await page.getByRole('navigation',{name:'Hidden agents',exact:true}).getByRole('button',{name:'Restore Delete fixture',exact:true}).click();
    await row.waitFor();
    await page.waitForFunction(element => element.querySelector('.bot-row__preview')?.textContent === '', await row.elementHandle());
    await row.click({button:'right'});await page.getByRole('menuitem',{name:'Edit',exact:true}).click();
    const profile=page.getByRole('dialog',{name:'Agent profile',exact:true});await profile.waitFor();
    await profile.getByRole('button',{name:'Agent profile actions',exact:true}).click();
    await page.getByRole('menuitem',{name:'Delete agent',exact:true}).click();
    let confirm=page.getByRole('alertdialog');await confirm.waitFor();
    assert.ok(await confirm.locator('.k-dialog__head').evaluate(el=>parseFloat(getComputedStyle(el).paddingLeft)>=16),'Confirm dialog lost its inset padding');
    assert.match(await confirm.textContent(),/Delete Delete fixture\?/);
    await confirm.getByRole('button',{name:'Cancel',exact:true}).click();
    await profile.getByRole('button',{name:'Cancel',exact:true}).click();
    await row.click();await row.click({button:'right'});await page.getByRole('menuitem',{name:'Delete',exact:true}).click();
    confirm=page.getByRole('alertdialog');await confirm.waitFor();
    await page.screenshot({path:join(evidence,'confirm.png'),animations:'disabled'});
    let release;
    const gate=new Promise(resolve=>{release=resolve;}), deletion=`**/api/agents/${id}/delete`;
    const hold=async route=>{await gate;await route.continue();};
    await page.route(deletion,hold);
    try {
      await confirm.getByRole('button',{name:'Delete agent',exact:true}).click();
      await page.getByRole('status').getByText('Deleting agent...',{exact:true}).waitFor();
      assert.equal(await page.getByRole('main',{name:'Conversation Delete fixture',exact:true}).count(),0);
    } finally {release();}
    await confirm.waitFor({state:'hidden'});await row.waitFor({state:'hidden'});
    await page.unroute(deletion,hold);
    assert.equal((await page.request.get(origin+`/api/agents/${id}/turns`)).status(),404);
    await page.reload();await row.waitFor({state:'hidden'});
    const agents=await (await page.request.get(origin+'/api/agents')).json();assert(!agents.agents.some(agent=>agent.id===id));
  });
  await check('Agent deletion aborts pending chat polls', async () => {
    const response = await page.request.post(origin + '/api/agents', { headers: { Origin: origin }, data: { runtime: 'api', id: randomUUID(), name: 'Delete polling fixture', model: 'smoke-primary', instructions: 'Help' } });
    assert.equal(response.status(), 201); const { id } = await response.json();
    await page.reload(); await page.getByRole('tab', { name: /^Agents/ }).click();
    const row = page.getByRole('button', { name: 'Delete polling fixture Idle', exact: true });
    const matches = request => new RegExp(`/api/agents/${id}/(?:dispatch|turns)\\??$`).test(request.url());
    let unblock, ready, ended; const gate = new Promise(resolve => { unblock = resolve; }), waiting = new Promise(resolve => { ready = resolve; }), finished = new Promise(resolve => { ended = resolve; });
    const wait = promise => Promise.race([promise, page.waitForTimeout(10_000).then(() => assert.fail('Pending chat polls did not settle'))]);
    const results = []; let pending = 0;
    const complete = value => { results.push(value); if (results.length === 2) ended(); };
    const failed = request => { if (matches(request)) complete(request.failure()?.errorText); };
    const received = response => { if (matches(response.request())) complete(response.status()); };
    const pattern = new RegExp(`/api/agents/${id}/(?:dispatch|turns)\\??$`);
    const hold = async route => { if (++pending === 2) ready(); await gate; await route.continue(); };
    page.on('requestfailed', failed); page.on('response', received); await page.route(pattern, hold);
    try {
      await row.click(); await wait(waiting);
      await row.click({ button: 'right' }); await page.getByRole('menuitem', { name: 'Delete', exact: true }).click();
      await page.getByRole('alertdialog').getByRole('button', { name: 'Delete agent', exact: true }).click();
      await row.waitFor({ state: 'hidden' }); unblock(); await wait(finished);
      assert.deepEqual(results, ['net::ERR_ABORTED', 'net::ERR_ABORTED'], 'Unmount must abort pending chat reads before deleted-agent responses arrive');
    } finally { unblock(); await page.unroute(pattern, hold); page.off('requestfailed', failed); page.off('response', received); }
  });
  if(viewport)await page.setViewportSize(viewport);
}

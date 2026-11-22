import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir, writeFile, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { desktopImage, installVncFixture, preloadVncFixture } from './computer-smoke.mjs';

export function selfVerifyReply(body) {
  const task = body.messages.findLast(message => message.role === 'user')?.content;
  if (typeof task !== 'string' || !task.startsWith('SV1 ')) return null;
  const call = (name, args) => ({ choices: [{ finish_reason: 'tool_calls', message: { tool_calls: [{ id: randomUUID(), type: 'function', function: { name, arguments: JSON.stringify(args) } }] } }] });
  const answer = content => ({ choices: [{ finish_reason: 'stop', message: { content } }] });
  if (task.startsWith('SV1 computer ')) return answer(`[Browser result](${task.slice(13)})`);
  const tools = body.messages.filter(message => message.role === 'tool');
  if (!tools.length) return call('run_shell', { command: 'node --test self-verify.test.mjs' });
  const results = tools.map(message => JSON.parse(message.content));
  if (results.length === 1) return call('create_text_file', { name: 'build-report.md', text: `# Build report\n\n${results[0].exitCode === 0 ? 'Checks passed.' : 'Checks failed.'}\n\n${results[0].output}` });
  if (results.length === 2) return call('verify_result', { kind: 'code', review: 'Request reread: native check completed and readable build report saved.' });
  return answer(`Read [Build report](${results[1].url}).`);
}

export async function selfVerifySmoke({ page, origin, root, check, gitAgent, gitName, gitWorkspace }) {
  const output = join(root, 'app/test-support/ui-smoke/artifacts/self-verify'); await mkdir(output, { recursive: true });
  const base = `/api/agents/${gitAgent}`;
  const post = async (path, data) => { const response = await page.request.post(origin + path, { headers: { Origin: origin }, data }); assert(response.ok(), await response.text()); return response.json(); };
  const details = page.getByRole('complementary', { name: 'Conversation details', exact: true });
  await writeFile(join(gitWorkspace, 'self-verify.test.mjs'), 'import assert from "node:assert/strict"; assert.equal(2 + 2, 4);');
  await check('SV1 checked report opens from inline prose; evidence stays in collapsed log', async () => {
    await page.goto(`${origin}/agents/${gitAgent}`); await page.getByRole('textbox', { name: `Message ${gitName}`, exact: true }).waitFor();
    const id = randomUUID(); await post(base + '/turns', { id, text: 'SV1 run checks and deliver report' });
    await page.getByRole('region', { name: 'Coding harness approval', exact: true }).getByRole('button', { name: 'Allow once', exact: true }).click();
    const turn = page.locator('.turn-block').filter({ has: page.getByText('SV1 run checks and deliver report', { exact: true }) });
    const answer = turn.getByLabel('Agent reply', { exact: true }), link = answer.locator('.markdown p a', { hasText: 'Build report' }); await link.waitFor();
    assert.equal(await answer.locator('[class*="chip"], .attachment, .k-badge').count(), 0);
    assert.equal(await answer.getByText('Run shell', { exact: true }).count(), 0);
    await link.click(); await details.getByRole('heading', { name: 'Build report', exact: true }).waitFor();
    await page.reload(); await details.getByRole('heading', { name: 'Build report', exact: true }).waitFor();
    const restored = page.locator('.turn-block').filter({ has: page.getByText('SV1 run checks and deliver report', { exact: true }) });
    const trigger = restored.locator('.turn-activity .k-collapsible__trigger'); assert.equal(await trigger.getAttribute('aria-expanded'), 'false');
    const run = restored.getByRole('button', { name: 'Run shell Completed', exact: true });
    assert.equal(await run.count(), 0);
    await trigger.click();
    await run.click();
    const result = details.getByRole('list', { name: 'Steps', exact: true }).getByRole('listitem').filter({ hasText: 'Run shell' });
    await result.locator('details[open]').waitFor();
    assert.match(await result.locator('pre').last().textContent(), /"exitCode":\s*0\b/);
    await page.screenshot({ path: join(output, 'checked-report.png'), fullPage: true, animations: 'disabled' });
  });
  await check('SV1 Computer prose link navigates URL immediately and after reload', async () => {
    const image = await desktopImage;
    const status = { available: true, enabled: true, state: 'running', revision: 1, width: 1280, height: 800, requests: [], control: { mode: 'agent', who: 'fixture', since: new Date().toISOString(), revision: 0 } };
    const pattern = /\/api\/agents\/[^/]+\/computer(?:\/[^?]+)?(?:\?.*)?$/;
    const navigated = [], target = 'https://example.test/result';
    await page.context().route(pattern, route => {
      const path = new URL(route.request().url()).pathname;
      if (path.endsWith('/open-url')) { navigated.push(route.request().postDataJSON().url); return route.fulfill({ json: { status: 'done' } }); }
      return route.fulfill({ json: path.endsWith('/vnc-ticket') ? { ticket: 'smoke-single-use' } : status });
    });
    const imageURL = `data:image/png;base64,${image.toString('base64')}`;
    await installVncFixture(page, imageURL); await preloadVncFixture(page.context(), imageURL);
    try {
      const href = `/agents/${gitAgent}?${new URLSearchParams({ result: JSON.stringify({ view: 'computer', agent: gitAgent, path: target, name: 'Browser result' }) })}`;
      await post(base + '/turns', { id: randomUUID(), text: 'SV1 computer ' + href });
      const link = page.locator('.agent-message .markdown p a').filter({ hasText: 'Browser result' }); await link.waitFor(); await link.click();
      await details.locator('.computer-viewer canvas').waitFor(); assert.equal(await details.getByRole('tab', { name: 'Computer', exact: true }).getAttribute('aria-selected'), 'true'); assert.deepEqual(navigated, [target]);
      await page.reload(); await details.locator('.computer-viewer canvas').waitFor(); assert.deepEqual(navigated, [target, target]);
      assert.equal(await page.locator('.agent-message [class*="chip"], .agent-message .attachment').count(), 0);
      await page.screenshot({ path: join(output, 'computer-result.png'), fullPage: true, animations: 'disabled' });
    } finally { await page.context().unroute(pattern); await rm(join(gitWorkspace, 'self-verify.test.mjs')); }
  });
}

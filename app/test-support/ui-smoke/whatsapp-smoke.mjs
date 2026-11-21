import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

export async function whatsappSmoke({ check, page, origin, settings, screenshot }) {
  const qr = 'data:image/svg+xml;base64,' + (await readFile(new URL('./whatsapp-fixture.svg', import.meta.url))).toString('base64');
  const root = origin + '/api/whatsapp';
  const status = { state: 'disconnected', connected: false, qr: null, chats: [], deliveryProblems: [], transcription: null };
  const fixture = route => {
    if (route.request().url() === root + '/link') Object.assign(status, { state: 'pairing', qr });
    if (route.request().url() === root + '/unlink') Object.assign(status, { state: 'disconnected', connected: false, qr: null });
    return route.fulfill({ json: status });
  };
  await check('WhatsApp QR pairing', async () => {
    await page.route(root, fixture);
    await page.route(root + '/*', fixture);
    try {
      const panel = await settings('Connected apps');
      const opener = panel.getByRole('button', { name: /^WhatsApp/ });
      await opener.focus();
      await page.keyboard.press('Enter');
      const dialog = page.getByRole('dialog', { name: 'WhatsApp', exact: true });
      await dialog.getByText('Unofficial WhatsApp Web link. Protocol updates may break it, and WhatsApp can ban your number.').waitFor();
      await dialog.getByRole('button', { name: 'Link WhatsApp', exact: true }).click();
      const image = dialog.getByRole('img', { name: 'WhatsApp linking QR code' });
      await image.waitFor();
      await image.evaluate(img => img.decode());
      assert(await image.evaluate(img => img.naturalWidth > 0));
      for (const theme of ['dark', 'light']) {
        await page.evaluate(value => { document.documentElement.dataset.theme = value; }, theme);
        await screenshot(`whatsapp-qr-${theme}`);
      }
      Object.assign(status, { state: 'linked', connected: true, qr: null });
      await dialog.getByRole('status').getByText('Linked', { exact: true }).waitFor();
      assert.equal(await image.count(), 0);
      await dialog.getByRole('button', { name: 'Unlink WhatsApp', exact: true }).click();
      await page.getByRole('alertdialog').getByRole('button', { name: 'Unlink', exact: true }).click();
      await dialog.getByRole('status').getByText('Disconnected', { exact: true }).waitFor();
      await dialog.getByRole('button', { name: 'Close', exact: true }).click();
      await dialog.waitFor({ state: 'hidden' });
      await page.waitForFunction(element => document.activeElement === element, await opener.elementHandle());
    } finally {
      await page.unroute(root, fixture);
      await page.unroute(root + '/*', fixture);
    }
  });
}

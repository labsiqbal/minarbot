export const fixtureClock = Date.UTC(2026, 10, 23, 7);

// Render full tiles in grayscale AA. Headless LCD rasterization depends on layer
// history, even when the native dialog and its React counterpart have identical geometry.
export const parityChromiumArgs = ['--disable-lcd-text', '--disable-partial-raster'];

export function fixDate(page, time = fixtureClock, { tick = false } = {}) {
  return page.addInitScript(({ time, tick }) => {
    const now = () => time + (tick ? Math.floor(performance.now()) : 0);
    globalThis.Date = new Proxy(Date, {
      apply: target => new target(now()).toString(),
      construct: (target, args, constructor) => Reflect.construct(target, args.length ? args : [now()], constructor),
      get: (target, key, receiver) => key === 'now' ? now : Reflect.get(target, key, receiver),
    });
  }, { time, tick });
}

export async function parityShot(page, path, region = page) {
  await page.evaluate(async () => {
    await document.fonts.ready;
    await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
  });
  return region.screenshot({ path, animations: 'disabled', caret: 'hide', scale: 'css' });
}

export async function withFixtureId(page, id, action) {
  await page.evaluate(id => {
    crypto.randomUUID = () => {
      delete crypto.randomUUID;
      return id;
    };
  }, id);
  try {
    await action();
  } finally {
    await page.evaluate(() => { delete crypto.randomUUID; });
  }
}

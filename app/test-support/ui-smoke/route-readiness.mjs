export async function routeReadiness(browser, options, url, selector, samples) {
  const cold = [], warm = [];
  for (let sample = 0; sample < samples; sample++) {
    const context = await browser.newContext(options);
    try {
      const page = await context.newPage();
      for (const [navigate, times] of [
        [() => page.goto(url, { waitUntil: 'commit' }), cold],
        [() => page.reload({ waitUntil: 'commit' }), warm],
      ]) {
        await navigate();
        const ready = await page.waitForFunction(selector => {
          const element = document.querySelector(selector);
          return element?.getClientRects().length && !element.disabled ? performance.now() : false;
        }, selector);
        times.push(await ready.jsonValue());
      }
    } finally {
      await context.close();
    }
  }
  const summary = times => {
    times.sort((a, b) => a - b);
    const percentile = p => +times[Math.ceil(times.length * p) - 1].toFixed(1);
    return { samples: times.length, median: percentile(0.5), p95: percentile(0.95) };
  };
  return { cold: summary(cold), warm: summary(warm) };
}

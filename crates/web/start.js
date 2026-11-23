const status = document.getElementById('appearance-startup');
const fallback = document.getElementById('appearance-fallback');
const title = fallback.querySelector('h1')?.textContent ?? 'Conversation';
let glue;

for (const cache of ['default', 'reload']) {
  try {
    const module = await import('./__APP__.js');
    const response = await fetch('__WASM_URL__', { cache, signal: AbortSignal.timeout(4000) });
    if (!response.ok) throw new Error('Minarbot download failed');
    await module.default({ module_or_path: response });
    glue = module;
    break;
  } catch {
    if (status) status.textContent = `Retrying ${title}…`;
  }
}

if (glue) {
  try {
    const pairing = document.querySelector('#leptos-root[data-pairing]');
    const editing = pairing && (fallback.contains(document.activeElement)
      || Array.from(fallback.querySelectorAll('input')).some(input => input.value));
    if (!editing) {
      __ADAPTER__
      glue.__MOUNT__();
      fallback.hidden = true;
      // Cached documents can still contain data from a revoked browser session.
      window.addEventListener('pagehide', event => {
        if (event.persisted) document.getElementById('leptos-root').hidden = true;
      });
      window.addEventListener('pageshow', event => {
        if (event.persisted) location.reload();
      });
    }
  } catch {
    document.getElementById('leptos-root').replaceChildren();
    glue = undefined;
  }
}
if (!glue && status) status.textContent = `${title} could not load. Reload this page to retry.`;

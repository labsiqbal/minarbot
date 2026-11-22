import init, { mount_appearance } from './appearance.js';

const status = document.getElementById('appearance-startup');
const title = document.querySelector('#appearance-fallback h1').textContent;
let ready = false;

for (const cache of ['default', 'reload']) {
  try {
    const response = await fetch('__WASM_URL__', { cache, signal: AbortSignal.timeout(4000) });
    if (!response.ok) throw new Error('Appearance download failed');
    await init({ module_or_path: response });
    ready = true;
    break;
  } catch {
    if (status) status.textContent = `Retrying ${title}…`;
  }
}

if (ready) {
  try {
    const fallback = document.getElementById('appearance-fallback');
    const pairing = document.querySelector('#leptos-root[data-pairing]');
    const editing = pairing && (fallback.contains(document.activeElement)
      || Array.from(fallback.querySelectorAll('input')).some(input => input.value));
    if (!editing) {
      mount_appearance();
      fallback.hidden = true;
    }
  } catch {
    document.getElementById('leptos-root').replaceChildren();
    ready = false;
  }
}
if (!ready && status) status.textContent = `${title} could not load. Open standard ${title} to continue.`;

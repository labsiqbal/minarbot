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
    status.textContent = `Retrying ${title}…`;
  }
}

if (ready) {
  try {
    mount_appearance();
    document.getElementById('appearance-fallback').hidden = true;
  } catch {
    document.getElementById('leptos-root').replaceChildren();
    ready = false;
  }
}
if (!ready) status.textContent = `${title} could not load. Open standard ${title} to continue.`;

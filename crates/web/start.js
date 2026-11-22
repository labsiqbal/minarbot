import init, { mount_appearance, mount_conversation } from './appearance.js';

const status = document.getElementById('appearance-startup');
const conversation = document.body.dataset.view === 'conversation';
const label = conversation ? 'conversation' : 'Appearance';
let ready = false;

for (const cache of ['default', 'reload']) {
  try {
    const response = await fetch('__WASM_URL__', { cache, signal: AbortSignal.timeout(4000) });
    if (!response.ok) throw new Error(label + ' download failed');
    await init({ module_or_path: response });
    ready = true;
    break;
  } catch {
    status.textContent = 'Retrying ' + label + '…';
  }
}

if (ready) {
  try {
    if (conversation) {
      const { browserAdapter } = await import('minarbot-conversation-adapter');
      globalThis.MinarbotConversation = browserAdapter;
      mount_conversation();
    } else {
      mount_appearance();
    }
    document.getElementById('appearance-fallback').hidden = true;
  } catch {
    document.getElementById('leptos-root').replaceChildren();
    ready = false;
  }
}
if (!ready) status.textContent = label + ' could not load.';

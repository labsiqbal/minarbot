const form = document.querySelector('form');
const input = document.querySelector('input');
const button = document.querySelector('button');
const error = document.querySelector('#error');
const status = document.querySelector('#status');
const params = new URLSearchParams(location.search);

status.textContent = params.get('status') ?? '';
error.textContent = params.get('error') ?? '';

form.addEventListener('submit', async (event) => {
  event.preventDefault();
  error.textContent = '';
  status.textContent = 'Connecting…';
  button.disabled = true;
  try {
    await window.__TAURI_INTERNALS__.invoke('connect_server', { address: input.value });
  } catch (message) {
    status.textContent = '';
    error.textContent = String(message);
    input.focus();
  } finally {
    button.disabled = false;
  }
});

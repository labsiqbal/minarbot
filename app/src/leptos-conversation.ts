import { createSpeechPlayer, voiceKey } from '../shared/speech';
import { speechPreferenceStore } from '../shared/speech-preference';
import { messageRanges } from './ui/messageFindRanges';
import { turnActivitySteps } from './ui/transcript';
import { forwardWheelToTimeline } from './ui/timelineMinimapWheel';
import { resolveTimelineMinimapPreview } from './ui/timelineMinimapItems';
import { marked, type Tokens } from 'marked';
import { markdownLink } from './markdown-link';
import { splitMarkdownBlocks } from './ui/markdownStreamBlocks';
import { bindHtmlReply } from './html-reply-bridge';
import { readHtmlReply } from './html-reply';
import { bindMcpApp } from './mcp-app-bridge';
import { readMcpAppReference } from './mcp-app-host';
import type RFB from '@novnc/novnc';
import { HttpError, readJson } from './http';
import { copyText, readClipboardText } from './ui/clipboard';
import { uploadLimitError } from './attachments';
import { startAudioRecording } from './audio-recording';

const escape = (value: string) => value.replaceAll('&', '&amp;').replaceAll('<', '&lt;')
  .replaceAll('>', '&gt;').replaceAll('"', '&quot;');

function markdownBlocks(text: string) {
  return text.length > 64000 ? [text] : splitMarkdownBlocks(text) ?? [text];
}

function markdown(text: string, scope: string) {
  const lines = text.split(/\r\n|\r|\n/);
  const columns = lines.reduce((max, line) => Math.max(max, line.split('|').length), 1);
  if (text.length > 64000 || columns * lines.length > 6000) return escape(text);
  const renderer = {
    html: ({ text }: Tokens.HTML | Tokens.Tag) => escape(text),
    image: ({ text }: Tokens.Image) => escape(text ? '[Image: ' + text + ']' : '[Image]'),
    link(this: { parser: { parseInline: (tokens: Tokens.Link['tokens']) => string } }, token: Tokens.Link) {
      const label = this.parser.parseInline(token.tokens);
      const target = markdownLink(token.href, scope, location.origin);
      if (!target) return label;
      const attributes = target.external ? ' target="_blank" rel="noopener noreferrer"' : target.result ? '' : ' download';
      return '<a href="' + escape(target.href) + '"' + attributes + '>' + label + '</a>';
    },
  };
  try {
    return marked.parse(text, { async: false, renderer: Object.assign(new marked.Renderer(), renderer) });
  } catch { return escape(text); }
}

async function upload(path: string, file: File) {
  const problem = uploadLimitError([file], 0);
  if (problem) throw problem;
  const key = 'minarbot:upload:' + path;
  const digest = [...new Uint8Array(await crypto.subtle.digest('SHA-256', await file.arrayBuffer()))]
    .map(byte => byte.toString(16).padStart(2, '0')).join('');
  const raw = sessionStorage.getItem(key);
  const previous = raw === null ? null : JSON.parse(raw) as { id?: string; name?: string; size?: number; digest?: string };
  if (raw !== null && (!previous || !/^[a-f0-9-]{36}$/i.test(previous.id ?? '') || previous.name !== file.name || previous.size !== file.size || previous.digest !== digest)) {
    throw 'Select original file to retry pending upload.';
  }
  const request = previous ?? { id: crypto.randomUUID(), name: file.name, size: file.size, digest };
  sessionStorage.setItem(key, JSON.stringify(request));
  try {
    const response = await fetch(`${path}/${request.id}`, { method: 'POST', headers: { 'Content-Type': 'application/octet-stream', 'X-File-Name': encodeURIComponent(file.name) }, body: file });
    const result = await readJson<{ id: string }>(response, 'Upload failed.');
    if (result.id !== request.id) throw 'Upload receipt does not match pending file.';
    sessionStorage.removeItem(key);
    return result.id;
  } catch (error) {
    if (error instanceof HttpError && error.status < 500 && error.status !== 409) sessionStorage.removeItem(key);
    throw error instanceof Error ? error.message : String(error);
  }
}

function ask(title: string, text: string, label: string, signal: AbortSignal) {
  if (signal.aborted || document.querySelector('dialog[open], [role="dialog"], [role="alertdialog"]')) return Promise.resolve(false);
  return new Promise<boolean>(resolve => {
    const dialog = document.createElement('dialog');
    dialog.className = 'conversation-dialog k-stack';
    dialog.setAttribute('role', 'alertdialog');
    dialog.setAttribute('aria-label', title);
    const heading = document.createElement('h2');
    heading.textContent = title;
    const detail = document.createElement('pre');
    detail.className = 'mcp-app-approval';
    detail.textContent = text;
    const form = document.createElement('form');
    form.method = 'dialog';
    form.className = 'k-row-actions';
    for (const [name, value] of [['Cancel', 'cancel'], [label, 'approve']]) {
      const button = document.createElement('button');
      button.className = 'k-btn k-btn--sm ' + (value === 'approve' ? 'k-btn--primary' : 'k-btn--secondary');
      button.value = value;
      button.textContent = name;
      form.append(button);
    }
    const abort = () => dialog.close();
    const finish = (approved: boolean) => {
      signal.removeEventListener('abort', abort);
      dialog.remove();
      resolve(approved);
    };
    dialog.addEventListener('close', () => finish(dialog.returnValue === 'approve' && !signal.aborted), { once: true });
    signal.addEventListener('abort', abort, { once: true });
    dialog.append(heading, detail, form);
    document.body.append(dialog);
    try { dialog.showModal(); } catch { finish(false); }
  });
}

function frame(wrapper: HTMLElement, scope: string, raw: string, mcp: boolean, action: (raw: string) => void) {
  const data: unknown = JSON.parse(raw);
  const reply = mcp ? readMcpAppReference(data) : readHtmlReply(data);
  if (!reply) return () => {};
  const channel = crypto.randomUUID();
  const iframe = document.createElement('iframe');
  const path = scope + (mcp ? '/mcp-apps/' : '/html-replies/') + reply.id;
  iframe.className = mcp ? 'html-reply mcp-app-frame' : 'html-reply';
  iframe.title = reply.title;
  iframe.sandbox.add('allow-scripts', 'allow-forms');
  iframe.referrerPolicy = 'no-referrer';
  iframe.loading = 'lazy';
  iframe.height = String('height' in reply ? reply.height : 240);
  const setHeight = (height: number) => { iframe.height = String(height); };
  const status = document.createElement('p');
  status.className = 'message-muted';
  status.setAttribute('role', 'status');
  status.textContent = 'Loading ' + reply.title + '…';
  if (mcp) wrapper.append(status);
  const dispose = mcp ? bindMcpApp(iframe, path, channel, reply.id,
    () => async data => action(JSON.stringify(data)), ask,
    () => { status.hidden = true; }, text => { status.textContent = text; status.hidden = false; iframe.hidden = true; }, setHeight)
    : bindHtmlReply(iframe, { ...reply, height: Number(iframe.height) }, channel, setHeight, value => action(JSON.stringify(value)));
  iframe.src = path + '#bridge=' + channel;
  wrapper.append(iframe);
  return () => { dispose(); wrapper.replaceChildren(); };
}

function mountVnc(target: HTMLElement, agent: string, revision: bigint, control: boolean, status: (raw: string) => void) {
  const abort = new AbortController();
  let socket: WebSocket | undefined;
  let client: RFB | undefined;
  const report = (message: string) => { if (!abort.signal.aborted) status(JSON.stringify({ status: message })); };
  const dispose = () => {
    abort.abort();
    if (client) client.disconnect();
    else if (socket && socket.readyState < WebSocket.CLOSING) socket.close();
    target.replaceChildren();
  };
  const fail = (message: string) => { report(message); dispose(); };
  const timeout = AbortSignal.timeout(15_000);
  const timedOut = () => fail('Screen connection timed out.');
  timeout.addEventListener('abort', timedOut, { once: true, signal: abort.signal });
  report('Connecting to computer…');
  void import('@novnc/novnc').then(async ({ default: Rfb }) => {
      const response = await fetch(`/api/agents/${agent}/computer/vnc-ticket`, {
        signal: abort.signal, method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ mode: control ? 'control' : 'watch', revision: Number(revision) }),
      });
      const { ticket } = await readJson<{ ticket: string }>(response);
      if (abort.signal.aborted) return;
      const url = new URL(`/api/agents/${agent}/computer/vnc`, location.href);
      url.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
      url.searchParams.set('ticket', ticket);
      const channel = new WebSocket(url, ['minarbot-vnc-v1', 'binary']);
      socket = channel;
      channel.binaryType = 'arraybuffer';
      channel.addEventListener('open', () => {
        const opened = performance.now();
        const attach = (slow: boolean) => {
          if (abort.signal.aborted) return;
          client = new Rfb(target, channel, { shared: true });
          if (slow) { client.qualityLevel = 0; client.compressionLevel = 6; }
          client.viewOnly = !control; client.scaleViewport = true; client.resizeSession = false; client.background = 'transparent'; client.focusOnClick = control;
          client.addEventListener('connect', () => { timeout.removeEventListener('abort', timedOut); report('Live'); }, { once: true, signal: abort.signal });
          client.addEventListener('clipboard', event => {
            const text = (event as CustomEvent<{ text?: unknown }>).detail?.text;
            if (!control || typeof text !== 'string') return;
            if (text.length <= 4000) status(JSON.stringify({ clipboard: text }));
            else report('Clipboard text exceeds 4000 characters.');
          }, { signal: abort.signal });
        };
        if (channel.protocol === 'minarbot-vnc-v1') channel.addEventListener('message', () => attach(performance.now() - opened > 130), { once: true, signal: abort.signal });
        else attach(false);
      }, { once: true, signal: abort.signal });
      channel.addEventListener('close', event => fail(event.code === 4408 ? 'Viewer limit reached.' : event.code === 4409 ? 'Desktop stopped.' : 'Screen connection closed.'), { once: true, signal: abort.signal });
    }).catch(error => {
      if (!abort.signal.aborted) fail(error instanceof HttpError && error.code === 'viewer_limit' ? 'Viewer limit reached.' : 'Screen connection failed.');
    });
  return dispose;
}

function record(state: (value: string) => void, file: (value: File) => void, error: (value: string) => void) {
  const session = startAudioRecording(state, file, error);
  return [session.stop, session.cancel];
}

const messageTime = (value: string) => new Date(value).toLocaleTimeString('en-US', { hour: '2-digit', minute: '2-digit' });

function findMatches(viewport: HTMLElement, query: string) {
  const ranges = messageRanges(viewport, query);
  if ('highlights' in CSS) {
    const highlights = new Highlight();
    for (const range of ranges) highlights.add(range);
    CSS.highlights.set('message-find', highlights);
  }
  return ranges;
}

function selectMatch(viewport: HTMLElement, range: Range | undefined, scroll: boolean) {
  if ('highlights' in CSS) CSS.highlights.set('message-find-current', new Highlight(...(range ? [range] : [])));
  if (scroll && range) {
    const top = parseFloat(getComputedStyle(viewport).scrollPaddingTop);
    viewport.scrollTop += range.getBoundingClientRect().top - viewport.getBoundingClientRect().top - top - 16;
  }
}

function clearMatches() {
  if ('highlights' in CSS) {
    CSS.highlights.delete('message-find');
    CSS.highlights.delete('message-find-current');
  }
}

function bindTurnTabStops(transcript: HTMLElement) {
  const actions = 'article.turn-block :is(button, a[href], input, textarea, select, [contenteditable="true"], summary)';
  const reset = (element: HTMLElement) => {
    if (element.tabIndex !== -1 && !element.closest('form, dialog, [role="menu"], [role="listbox"]')) element.tabIndex = -1;
  };
  const observer = new MutationObserver(records => {
    for (const record of records) for (const node of record.addedNodes) {
      if (!(node instanceof HTMLElement)) continue;
      if (node.matches(actions)) reset(node);
      node.querySelectorAll<HTMLElement>(actions).forEach(reset);
    }
  });
  observer.observe(transcript, { childList: true, subtree: true });
  transcript.querySelectorAll<HTMLElement>(actions).forEach(reset);
  return () => observer.disconnect();
}

function postKeepalive(path: string, body: string) {
  void fetch(path, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body, keepalive: true }).catch(() => {});
}

function workLog(turn: string, name: string) {
  return JSON.stringify(turnActivitySteps(JSON.parse(turn), name));
}

function bindTimelineWheel(rail: HTMLElement, viewport: HTMLElement, navigate: () => void) {
  const handler = (event: WheelEvent) => {
    if (event.defaultPrevented || event.ctrlKey || !event.deltaY) return;
    navigate();
    forwardWheelToTimeline(event, viewport);
  };
  rail.addEventListener('wheel', handler, { passive: false });
  return () => rail.removeEventListener('wheel', handler);
}

function minimapPreview(text: string) {
  return resolveTimelineMinimapPreview({ id: '', rowIndex: 0, userText: null, assistantText: text })?.assistantText ?? '';
}

function speechPlayback(listener: (enabled: boolean, owner: string, playing: boolean, error: string) => void) {
  const engine = 'speechSynthesis' in window && 'SpeechSynthesisUtterance' in window ? window.speechSynthesis : null;
  if (!engine) return null;
  const player = createSpeechPlayer(engine, text => new SpeechSynthesisUtterance(text));
  const report = () => {
    const state = player.getSnapshot();
    listener(speechPreferenceStore.getSnapshot().enabled, state.owner, state.playing, state.error);
  };
  const update = player.subscribe(report);
  const preference = speechPreferenceStore.subscribe(() => { player.stop(); report(); });
  const hide = () => player.stop();
  window.addEventListener('pagehide', hide);
  report();
  return {
    play(owner: string, text: string) {
      const active = player.getSnapshot();
      if (active.owner === owner && active.playing) { player.stop(owner); return; }
      const selected = speechPreferenceStore.getSnapshot();
      if (!selected.enabled) return;
      try {
        if (!engine.getVoices().some(voice => voice.localService && voiceKey(voice) === selected.voice)) {
          listener(true, owner, false, 'Pick a voice in Settings > General.');
          return;
        }
        player.play(owner, text, selected.voice);
      } catch { listener(true, owner, false, 'Voice unavailable. Pick a voice in Settings > General.'); }
    },
    stop(owner: string) { player.stop(owner); },
    dispose() {
      update();
      preference();
      window.removeEventListener('pagehide', hide);
      player.stop();
    },
  };
}

export const browserAdapter = { findMatches, selectMatch, clearMatches, minimapPreview, bindTimelineWheel, bindTurnTabStops, postKeepalive, workLog, speechPlayback, record, messageTime, markdown, markdownBlocks, upload, mountFrame: frame, mountVnc, readClipboard: readClipboardText,
  writeClipboard: copyText };

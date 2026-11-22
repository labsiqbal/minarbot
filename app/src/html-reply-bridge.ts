import { createFrameQueue } from './hooks/frame-queue';
import { readHtmlReplyMessage, type HtmlReplyAction, type HtmlReplyReference } from './html-reply';


const themeTokens = {
  '--background': '--color-bg-pane',
  '--foreground': '--color-text-primary',
  '--muted': '--color-bg-active-solid',
  '--muted-foreground': '--color-text-secondary',
  '--card': '--color-bg-elevated',
  '--border': '--color-border-primary',
  '--primary': '--color-brand',
  '--primary-soft': '--color-brand-soft',
  '--primary-foreground': '--bot-on-action',
  '--accent': '--color-brand-text',
  '--ring': '--color-border-focus',
  '--chart-1': '--color-brand',
  '--radius': '--radius-md',
  '--font-sans': '--font-stack',
  '--text-sm': '--text-sm',
  '--font-mono': '--font-mono-stack',
};

export function bindHtmlReply(iframe: HTMLIFrameElement, reply: HtmlReplyReference, channel: string, setHeight: (height: number) => void, onAction?: (action: HtmlReplyAction) => void) {
  const root = document.documentElement;
  const system = matchMedia('(prefers-color-scheme: dark)');
  const theme = () => {
    const styles = getComputedStyle(iframe);
    const mode = root.dataset.theme === 'system' ? (system.matches ? 'dark' : 'light') : root.dataset.theme === 'dark' ? 'dark' : 'light';
    const variables = Object.fromEntries(Object.entries(themeTokens).map(([name, token]) => [name, styles.getPropertyValue(token).trim()]).filter(([, value]) => value));
    const chart = mode === 'dark' ? ['#5eead4', '#fbbf24', '#c084fc', '#fb7185', '#a3e635'] : ['#087f70', '#875400', '#8d2fc1', '#b11b56', '#4b7014'];
    chart.forEach((color, index) => { variables[`--chart-${index + 2}`] = color; });
    // Only public presentation tokens cross into the opaque frame.
    iframe.contentWindow?.postMessage({ channel, method: 'theme', mode, variables }, '*');
  };
  const last = new Map<string, number>();
  const frameQueue = createFrameQueue();
  let scheduled = false;
  let measured = reply.height;
  const receive = (event: MessageEvent) => {
    if (event.source !== iframe.contentWindow || event.origin !== 'null') return;
    const now = performance.now();
    const message = readHtmlReplyMessage(event, iframe.contentWindow, channel, reply.id);
    if (!message) return;
    if (message.kind !== 'size' && now - (last.get(message.kind) ?? -Infinity) < 250) return;
    last.set(message.kind, now);
    switch (message.kind) {
      case 'size':
        measured = message.height;
        if (!scheduled) {
          scheduled = true;
          frameQueue.schedule(() => { scheduled = false; setHeight(measured); });
        }
        break;
      case 'ready': theme(); break;
      case 'context': onAction?.(message); break;
      case 'draft':
      case 'link':
        if (document.activeElement !== iframe || !navigator.userActivation?.isActive) break;
        if (message.kind === 'draft') onAction?.(message);
        else window.open(message.href, '_blank', 'noopener,noreferrer');
    }
  };
  const observer = new MutationObserver(theme);
  observer.observe(root, { attributes: true, attributeFilter: ['data-theme', 'data-color-theme', 'style'] });
  system.addEventListener('change', theme);
  iframe.addEventListener('load', theme);
  window.addEventListener('message', receive);
  return () => {
    frameQueue.cancel();
    observer.disconnect();
    system.removeEventListener('change', theme);
    iframe.removeEventListener('load', theme);
    window.removeEventListener('message', receive);
  };
}

import { useLayoutEffect, useRef, useState } from 'react';
import { createFrameQueue } from './hooks/useNextFrame';
import { readHtmlReply, readHtmlReplyMessage, type HtmlReplyAction, type HtmlReplyReference } from './html-reply';
import './html-reply.css';

const themeTokens = {
  '--background': '--color-bg-pane',
  '--foreground': '--color-text-primary',
  '--muted': '--color-bg-active-solid',
  '--muted-foreground': '--color-text-secondary',
  '--card': '--color-bg-elevated',
  '--border': '--color-border-primary',
  '--primary': '--color-brand',
  '--primary-foreground': '--bot-on-action',
  '--accent': '--color-brand-text',
  '--ring': '--color-border-focus',
  '--chart-1': '--color-brand',
  '--radius': '--radius-md',
  '--font-sans': '--font-stack',
  '--font-mono': '--font-mono-stack',
};

export function HtmlReplies({ replies, scope, onAction }: {
  replies?: HtmlReplyReference[];
  scope: string;
  onAction?: (action: HtmlReplyAction) => void;
}) {
  return replies?.slice(0, 4).map(raw => {
    const reply = readHtmlReply(raw);
    return reply && <HtmlReply key={reply.id} reply={reply} scope={scope} onAction={onAction} />;
  });
}

function HtmlReply({ reply, scope, onAction }: { reply: HtmlReplyReference; scope: string; onAction?: (action: HtmlReplyAction) => void }) {
  const frame = useRef<HTMLIFrameElement>(null);
  const [channel] = useState(() => crypto.randomUUID());
  const [height, setHeight] = useState(reply.height);
  const action = useRef(onAction);
  useLayoutEffect(() => { action.current = onAction; }, [onAction]);

  useLayoutEffect(() => {
    const iframe = frame.current;
    if (!iframe) return;
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
        case 'context': action.current?.(message); break;
        case 'draft':
        case 'link':
          if (document.activeElement !== iframe || !navigator.userActivation?.isActive) break;
          if (message.kind === 'draft') action.current?.(message);
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
  }, [channel, reply.id, reply.height]);

  return <iframe
    ref={frame}
    className="html-reply"
    title={reply.title}
    src={`${scope}/html-replies/${reply.id}#bridge=${channel}`}
    sandbox="allow-scripts allow-forms"
    referrerPolicy="no-referrer"
    loading="lazy"
    height={height}
  />;
}

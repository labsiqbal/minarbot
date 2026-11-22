// Adapted from T3 Code's McpAppFrame (MIT, app/t3/NOTICE).
import { useLayoutEffect, useRef, useState } from 'react';
import { useConfirm } from './ui/kit';
import { readMcpAppReference, type McpAppReference } from './mcp-app-host';
import { bindMcpApp, type McpAppHandler } from './mcp-app-bridge';
export type { McpAppAction, McpAppHandler } from './mcp-app-bridge';
import './html-reply.css';

export function McpAppReplies({ apps, scope, onAction }: {
  apps?: McpAppReference[];
  scope: string;
  onAction?: McpAppHandler;
}) {
  return apps?.slice(0, 4).map(raw => {
    const app = readMcpAppReference(raw);
    return app && <McpAppFrame key={app.id} app={app} scope={scope} onAction={onAction} />;
  });
}

function McpAppFrame({ app, scope, onAction }: { app: McpAppReference; scope: string; onAction?: McpAppHandler }) {
  const ask = useConfirm();
  const action = useRef(onAction);
  useLayoutEffect(() => { action.current = onAction; }, [onAction]);
  const frame = useRef<HTMLIFrameElement>(null);
  const [channel] = useState(() => crypto.randomUUID());
  const [height, setHeight] = useState(240);
  const [ready, setReady] = useState(false);
  const [error, setError] = useState('');
  const path = `${scope}/mcp-apps/${app.id}`;

  useLayoutEffect(() => {
    const iframe = frame.current;
    if (!iframe) return;
    return bindMcpApp(iframe, path, channel, app.id, () => action.current,
      (title, text, label) => ask({ title, description: <span className="mcp-app-approval">{text}</span>, confirm: label }),
      setReady, setError, setHeight);
  }, [app.id, app.title, ask, channel, path]);

  return <>
    {error ? <p role="status" className="message-error">{error}</p> : !ready && <p role="status" className="message-muted">Loading {app.title}…</p>}
    <iframe ref={frame} className="html-reply mcp-app-frame" title={app.title} hidden={!!error}
      sandbox="allow-scripts allow-forms" referrerPolicy="no-referrer" loading="lazy"
      height={error ? 0 : height} src={`${path}#bridge=${channel}`} />
  </>;
}

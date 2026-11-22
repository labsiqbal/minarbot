// Adapted from T3 Code's McpAppFrame (MIT, app/t3/NOTICE).
import { useLayoutEffect, useRef, useState } from 'react';
import { useConfirm } from './ui/kit';
import { readJson } from './http';
import { createFrameQueue } from './hooks/useNextFrame';
import {
  appContext, appLink, appMessage, appTool, MCP_APP_MAX_HEIGHT, MCP_APP_PROTOCOL_VERSION,
  readMcpAppMessage, readMcpAppReference, type McpAppReference,
} from './mcp-app-host';
import './html-reply.css';

export type McpAppAction =
  | { kind: 'message'; text: string }
  | { kind: 'context'; id: string; data: string };
export type McpAppHandler = (action: McpAppAction) => Promise<void>;

type Bootstrap = { csp: object; hostVersion: string; server: string; tool: object; arguments: object; result: object };
type ToolReply = { status: 'approval'; id: string } | { status: 'complete'; result: object };

const styles = {
  '--color-background-primary': '--color-bg-pane',
  '--color-background-secondary': '--color-bg-elevated',
  '--color-background-tertiary': '--color-bg-active-solid',
  '--color-text-primary': '--color-text-primary',
  '--color-text-secondary': '--color-text-secondary',
  '--color-border-primary': '--color-border-primary',
  '--color-ring-primary': '--color-border-focus',
  '--font-sans': '--font-stack',
  '--font-mono': '--font-mono-stack',
  '--border-radius-md': '--radius-md',
};

class RpcError extends Error {
  readonly code: number;
  constructor(code: number, message: string) { super(message); this.code = code; }
}
const invalid = (message: string) => new RpcError(-32602, message);

let approvalOpen = false;
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
    const root = document.documentElement;
    const system = matchMedia('(prefers-color-scheme: dark)');
    const abort = new AbortController();
    const frameQueue = createFrameQueue();
    const pending = new Set<string | number>();
    let bootstrap: Bootstrap | undefined;
    let initializing = false;
    let initialized = false;
    let sentContext = '';
    let lastSecond = 0;
    let requests = 0;
    let loads = 0;
    let measured = 240;
    let measuring = false;

    const hostContext = () => {
      const theme = root.dataset.theme === 'system' ? (system.matches ? 'dark' : 'light')
        : root.dataset.theme === 'dark' ? 'dark' : 'light';
      const css = getComputedStyle(iframe);
      return {
        theme,
        styles: { variables: Object.fromEntries(Object.entries(styles).map(([key, token]) => [key, css.getPropertyValue(token).trim()])) },
        displayMode: 'inline',
        availableDisplayModes: ['inline'],
        containerDimensions: { width: iframe.clientWidth, maxHeight: MCP_APP_MAX_HEIGHT },
        platform: 'web',
        locale: navigator.language,
        ...(bootstrap ? { toolInfo: { tool: bootstrap.tool } } : {}),
      };
    };
    const post = (payload: object) => {
      // Opaque origins require "*" for delivery to the pinned Window.
      if (!abort.signal.aborted) iframe.contentWindow?.postMessage({ channel, rpc: { jsonrpc: '2.0', ...payload } }, '*');
    };
    const notify = (method: string, params: object) => post({ method, params });
    const reject = (id: string | number, code: number, message: string) => post({ id, error: { code, message } });
    const stop = (message: string) => {
      abort.abort();
      setError(message);
    };
    const themeChanged = () => {
      if (!initialized || abort.signal.aborted) return;
      const context = hostContext();
      const next = JSON.stringify(context);
      if (sentContext !== next) {
        sentContext = next;
        notify('ui/notifications/host-context-changed', context);
      }
    };
    async function request<T>(route: string, body?: object) {
      return readJson<T>(await fetch(path + route, {
        signal: abort.signal,
        ...(body ? { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) } : {}),
      }));
    }
    async function confirm(title: string, text: string, label: string) {
      if (approvalOpen || document.querySelector('[role="dialog"], [role="alertdialog"]')) {
        throw new Error('Finish the open dialog and try again.');
      }
      approvalOpen = true;
      try {
        const approved = await ask({ title, description: <span className="mcp-app-approval">{text}</span>, confirm: label });
        if (!approved || abort.signal.aborted) throw new Error('Request declined.');
      } finally {
        approvalOpen = false;
      }
    }

    async function run(method: string, params: Record<string, unknown>) {
      if (method === 'ping') return {};
      if (method === 'ui/initialize') {
        if (initializing || bootstrap || typeof params.protocolVersion !== 'string') throw invalid('Invalid app initialization.');
        initializing = true;
        try {
          bootstrap = await request<Bootstrap>('/data');
        } finally {
          initializing = false;
        }
        const context = hostContext();
        sentContext = JSON.stringify(context);
        return {
          protocolVersion: MCP_APP_PROTOCOL_VERSION,
          hostInfo: { name: 'Minarbot', version: bootstrap.hostVersion },
          hostCapabilities: {
            serverTools: {},
            openLinks: {},
            message: action.current ? { text: {} } : undefined,
            updateModelContext: action.current ? { text: {}, structuredContent: {} } : undefined,
            sandbox: { csp: bootstrap.csp },
          },
          hostContext: context,
        };
      }
      if (!initialized || !bootstrap) throw new RpcError(-32000, 'Initialize the MCP app first.');
      switch (method) {
        case 'tools/call': {
          const call = appTool(params);
          if (!call) throw invalid('Expected a tool name and object arguments.');
          const reply = await request<ToolReply>('/call', call);
          if (reply.status === 'complete') return reply.result;
          await confirm(`Allow ${bootstrap.server} to run ${call.name}?`, JSON.stringify(call.arguments, null, 2), 'Run tool');
          const approved = await request<ToolReply>('/call', { ...call, approval: reply.id });
          if (approved.status !== 'complete') throw new Error('Tool approval expired.');
          return approved.result;
        }
        case 'ui/message': {
          const text = appMessage(params);
          if (!text || !action.current) throw invalid('Expected a text message from the user role.');
          await confirm(`Send message from ${bootstrap.server}?`, text, 'Send message');
          const send = action.current;
          if (!send) throw new Error('Conversation is unavailable.');
          await send({ kind: 'message', text });
          return {};
        }
        case 'ui/update-model-context': {
          const data = appContext(params);
          if (data === undefined || !action.current) throw invalid('Expected text or structured context within 4 KiB.');
          await action.current({ kind: 'context', id: app.id, data });
          return {};
        }
        case 'ui/open-link': {
          const href = appLink(params.url);
          if (!href) throw invalid('Expected an HTTP or HTTPS link.');
          if (document.activeElement !== iframe || !navigator.userActivation?.isActive) throw new Error('Open links from the app.');
          window.open(href, '_blank', 'noopener,noreferrer');
          return {};
        }
        default: throw new RpcError(-32601, 'Method not supported.');
      }
    }
    const receive = (event: MessageEvent) => {
      if (abort.signal.aborted) return;
      const incoming = readMcpAppMessage(event, iframe.contentWindow, channel);
      if (!incoming) return;
      if (incoming === 'closed') { stop('MCP app closed after navigation.'); return; }
      const { id, method, params } = incoming;
      if (id === undefined) {
        if (method === 'ui/notifications/initialized' && bootstrap && !initialized) {
          initialized = true;
          setReady(true);
          notify('ui/notifications/tool-input', { arguments: bootstrap.arguments });
          notify('ui/notifications/tool-result', bootstrap.result);
          themeChanged();
        }
        if (initialized && method === 'ui/notifications/size-changed'
          && typeof params.height === 'number' && Number.isFinite(params.height) && params.height > 0) {
          measured = Math.min(MCP_APP_MAX_HEIGHT, Math.max(80, Math.ceil(params.height)));
          if (!measuring) {
            measuring = true;
            frameQueue.schedule(() => { measuring = false; setHeight(measured); });
          }
        }
        return;
      }
      const second = Math.floor(performance.now() / 1000);
      if (second !== lastSecond) { lastSecond = second; requests = 0; }
      if (pending.has(id)) return;
      if (++requests > 12 || pending.size >= 4) { reject(id, -32000, 'Too many requests.'); return; }
      pending.add(id);
      void run(method, params).then(
        result => post({ id, result }),
        (error: unknown) => reject(id, error instanceof RpcError ? error.code : -32000,
          error instanceof Error ? error.message : 'MCP app request failed.'),
      ).finally(() => pending.delete(id));
    };
    const loaded = () => {
      if (++loads > 1) stop('MCP app closed after navigation.');
      else if (!initialized) AbortSignal.timeout(12_000).addEventListener('abort', () => {
        if (!initialized) stop('MCP app did not start.');
      }, { signal: abort.signal, once: true });
    };
    const themeObserver = new MutationObserver(themeChanged);
    const sizeObserver = new ResizeObserver(themeChanged);
    themeObserver.observe(root, { attributes: true, attributeFilter: ['data-theme', 'data-color-theme', 'style'] });
    sizeObserver.observe(iframe);
    system.addEventListener('change', themeChanged);
    window.addEventListener('message', receive);
    iframe.addEventListener('load', loaded);
    return () => {
      abort.abort();
      frameQueue.cancel();
      themeObserver.disconnect();
      sizeObserver.disconnect();
      system.removeEventListener('change', themeChanged);
      window.removeEventListener('message', receive);
      iframe.removeEventListener('load', loaded);
    };
  }, [app.id, app.title, ask, channel, path]);

  return <>
    {error ? <p role="status" className="message-error">{error}</p> : !ready && <p role="status" className="message-muted">Loading {app.title}…</p>}
    <iframe ref={frame} className="html-reply mcp-app-frame" title={app.title} hidden={!!error}
      sandbox="allow-scripts allow-forms" referrerPolicy="no-referrer" loading="lazy"
      height={error ? 0 : height} src={`${path}#bridge=${channel}`} />
  </>;
}

import { useLayoutEffect, useRef, useState } from 'react';
import { bindHtmlReply } from './html-reply-bridge';
import { readHtmlReply, type HtmlReplyAction, type HtmlReplyReference } from './html-reply';
import './html-reply.css';

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
    return bindHtmlReply(iframe, reply, channel, setHeight, value => action.current?.(value));
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

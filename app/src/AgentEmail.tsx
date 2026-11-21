import { ChevronLeft, ChevronRight, Copy, Inbox, RefreshCw } from 'lucide-react';
import { useAgentEmailSettings, useAgentInbox, type EmailRequest, type EmailSummary } from './hooks/useAgentEmail';
import { Alert, Button, Card, Dialog, EmptyState, Field, IconButton, Input, PageHeader, Row, Section, SkeletonRows, useConfirm } from './ui/kit';
import './ui/agent-email.css';

const dateFormat = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' });
const subject = (item: EmailSummary) => item.subject || '(No subject)';
function Received({ value }: { value: number }) {
  return <time dateTime={new Date(value).toISOString()}>{dateFormat.format(value)}</time>;
}

export function AgentEmailSettings({ request }: { request: EmailRequest }) {
  const state = useAgentEmailSettings(request);
  const confirm = useConfirm();

  return <div className="k-page bs-root agent-email-settings">
    <PageHeader headingRef={state.heading} title="Email" />
    {state.error && <Alert tone="error" actions={<Button size="sm" onClick={state.reload}>Reload settings</Button>}>{state.error}</Alert>}
    {state.settings ? <>
      <form className="agent-email-settings__form" onSubmit={event => { event.preventDefault(); void state.save({ domain: state.domain.trim() }); }}>
        <Field label="Receiving domain">
          <Input name="domain" value={state.domain} onChange={event => state.setDomain(event.target.value)}
            maxLength={253} placeholder="agents.example.com" autoComplete="off" autoCapitalize="none"
            spellCheck={false} disabled={state.busy} />
        </Field>
        <Button type="submit" variant="primary" disabled={state.busy || state.domain.trim() === (state.settings.domain ?? '')}>Save domain</Button>
      </form>
      {state.notice && !state.secret && <span className="settings-fact" role="status">{state.notice}</span>}
      <Section title="Inbound relay">
        <Card>
          <Row title="Endpoint path" meta={<code>{state.settings.endpoint}</code>} />
          <Row title="Endpoint secret" meta={state.settings.hasSecret ? 'Configured' : 'Not generated'}
            trailing={<Button size="sm" disabled={state.busy || !state.settings.domain} onClick={async () => {
              if (state.settings?.hasSecret && !await confirm({ title: 'Rotate endpoint secret?', description: 'Update your relay with the new secret to resume email delivery.', confirm: 'Rotate secret', danger: true })) return;
              void state.save({ rotateSecret: true });
            }}>{state.settings.hasSecret ? 'Rotate secret' : 'Generate secret'}</Button>} />
        </Card>
      </Section>
    </> : !state.error && <><span className="sr-only" role="status">Loading email settings...</span><SkeletonRows /></>}
    <Dialog open={!!state.secret} onOpenChange={open => { if (!open) state.closeSecret(); }}
      title="Save endpoint secret" description="Shown once. Copy it to your relay before closing."
      footer={<Button variant="primary" onClick={state.closeSecret}>Done</Button>}>
      <Field label="Endpoint secret">
        <div className="k-inline">
          <Input data-private-token readOnly value={state.secret} autoComplete="off"
            spellCheck={false} onFocus={event => event.currentTarget.select()} />
          <IconButton label="Copy endpoint secret" onClick={() => void state.copySecret()}><Copy size={16} aria-hidden /></IconButton>
        </div>
      </Field>
      {state.notice && <span role="status">{state.notice}</span>}
    </Dialog>
  </div>;
}

export function AgentInbox({ agent, request, onBack, onSettings }: {
  agent: { id: string; name: string }; request: EmailRequest; onBack: () => void; onSettings: () => void;
}) {
  const state = useAgentInbox(agent.id, request);
  const page = state.page;
  return <div className="k-page bs-root agent-inbox">
    <PageHeader headingRef={state.heading} title="Inbox" description={agent.name} actions={<>
      <IconButton label="Refresh inbox" onClick={state.refresh}><RefreshCw size={16} aria-hidden /></IconButton>
      <Button variant="ghost" onClick={onBack}>Back to chat</Button>
    </>} />
    {page?.address ? <div className="agent-inbox__address">
      <code>{page.address}</code>
      <IconButton label="Copy email address" onClick={() => void state.copyAddress()}><Copy size={16} aria-hidden /></IconButton>
      <span className="sr-only" role="status">{state.copyNotice}</span>
    </div> : page && <Alert actions={<Button size="sm" onClick={onSettings}>Email settings</Button>}>Set a receiving domain to enable new mail.</Alert>}
    {state.listError && <Alert tone="error">{state.listError}</Alert>}
    <div className={`agent-inbox__layout${state.selected ? ' has-message' : ''}`}>
      <div className="agent-inbox__list">
        {page ? page.items.length ? <Card><ul>
          {page.items.map(item => <li key={item.id}>
            <button type="button" id={`inbox-message-${item.id}`} aria-current={state.selected === item.id ? 'true' : undefined} onClick={() => state.open(item.id)}>
              <strong>{subject(item)}</strong>
              <span>{item.from}</span>
              <Received value={item.received} />
            </button>
          </li>)}
        </ul></Card> : <EmptyState icon={<Inbox size={18} />} title="No messages" />
          : !state.listError && <><span className="sr-only" role="status">Loading inbox...</span><SkeletonRows /></>}
        {(state.before || page?.nextBefore) && <div className="k-pager">
          <IconButton label="Newer messages" disabled={!state.before} onClick={() => state.goToPage(state.cursors.slice(0, -1))}><ChevronLeft size={16} aria-hidden /></IconButton>
          <IconButton label="Older messages" disabled={!page?.nextBefore} onClick={() => { if (page?.nextBefore) state.goToPage([...state.cursors, page.nextBefore]); }}><ChevronRight size={16} aria-hidden /></IconButton>
        </div>}
      </div>
      {state.selected && <article className="agent-inbox__reader">
        <div className="agent-inbox__reader-head">
          <h2 ref={state.reader} tabIndex={-1}>{state.mail ? subject(state.mail) : 'Message'}</h2>
          <IconButton label="Back to message list" onClick={state.closeMessage}><ChevronLeft size={16} aria-hidden /></IconButton>
        </div>
        {state.readError ? <Alert tone="error">{state.readError}</Alert> : state.mail ? <>
          <dl>
            <dt>From</dt><dd>{state.mail.from}</dd>
            <dt>To</dt><dd>{state.mail.to}</dd>
            <dt>Received</dt><dd><Received value={state.mail.received} /></dd>
          </dl>
          <p className="agent-inbox__body">{state.mail.text || 'No message text.'}</p>
          {state.mail.textTruncated && <p className="k-field__hint">Showing the first 32,000 characters.</p>}
        </> : <p role="status">Loading message...</p>}
      </article>}
    </div>
  </div>;
}

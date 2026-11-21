import { useEffect, useRef, useState } from 'react';
import { usePolling } from './usePolling';
import { copyText } from '../ui/clipboard';

type EmailSettings = { domain: string | null; hasSecret: boolean; revision: number; endpoint: string };
export type EmailSummary = { id: string; from: string; subject: string; received: number };
type EmailPage = { address: string | null; items: EmailSummary[]; nextBefore: number | null };
type EmailMessage = EmailSummary & { to: string; text: string; textTruncated?: boolean };
export type EmailRequest = <T>(path: string, data?: unknown, signal?: AbortSignal) => Promise<T>;

const settingsPath = '/api/email/settings';
const message = (error: unknown) => error instanceof Error ? error.message : 'Email request failed.';

export function useAgentEmailSettings(request: EmailRequest) {
  const [settings, setSettings] = useState<EmailSettings | null>(null);
  const [domain, setDomain] = useState('');
  const [secret, setSecret] = useState('');
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [busy, setBusy] = useState(false);
  const [refresh, setRefresh] = useState(0);
  const heading = useRef<HTMLHeadingElement>(null);

  useEffect(() => { heading.current?.focus(); }, []);
  useEffect(() => {
    const controller = new AbortController();
    void request<EmailSettings>(settingsPath, undefined, controller.signal).then(value => {
      if (controller.signal.aborted) return;
      setSettings(value);
      setDomain(value.domain ?? '');
      setError('');
    }).catch(error => {
      if (!controller.signal.aborted) setError(message(error));
    });
    return () => controller.abort();
  }, [request, refresh]);

  async function save(change: { domain: string } | { rotateSecret: true }) {
    if (!settings || busy) return;
    setBusy(true); setError(''); setNotice('');
    try {
      const { secret: nextSecret, ...saved } = await request<EmailSettings & { secret?: string }>(settingsPath, {
        id: crypto.randomUUID(), revision: settings.revision, ...change,
      });
      setSettings(saved); setDomain(saved.domain ?? '');
      if (nextSecret) setSecret(nextSecret);
      else setNotice('Receiving domain saved.');
    } catch (error) { setError(message(error)); }
    finally { setBusy(false); }
  }

  async function copySecret() {
    try { await copyText(secret); setNotice('Copied.'); }
    catch { setNotice('Select the secret to copy it.'); }
  }

  return {
    settings, domain, setDomain, secret, error, notice, busy, heading, save, copySecret,
    closeSecret: () => { setSecret(''); setNotice(''); },
    reload: () => setRefresh(value => value + 1),
  };
}

export function useAgentInbox(agent: string, request: EmailRequest) {
  const base = `/api/agents/${agent}/email`;
  const [page, setPage] = useState<EmailPage | null>(null);
  const [cursors, setCursors] = useState<number[]>([]);
  const [refresh, setRefresh] = useState(0);
  const [listError, setListError] = useState('');
  const [selected, setSelected] = useState('');
  const [mail, setMail] = useState<EmailMessage | null>(null);
  const [readError, setReadError] = useState('');
  const [copyNotice, setCopyNotice] = useState('');
  const heading = useRef<HTMLHeadingElement>(null);
  const reader = useRef<HTMLHeadingElement>(null);
  const before = cursors.at(-1);

  useEffect(() => { heading.current?.focus(); }, []);
  useEffect(() => { if (mail || readError) reader.current?.focus(); }, [mail, readError]);
  usePolling(async signal => {
    try {
      const value = await request<EmailPage>(base + (before ? `?before=${before}` : ''), undefined, signal);
      if (!signal.aborted) { setPage(value); setListError(''); }
    } catch (error) { if (!signal.aborted) setListError(message(error)); }
  }, { interval: 5000 }, [base, before, refresh, request]);

  useEffect(() => {
    if (!selected) return;
    const controller = new AbortController();
    void request<EmailMessage>(`${base}/messages/${selected}`, undefined, controller.signal).then(value => {
      if (!controller.signal.aborted) { setMail(value); setReadError(''); }
    }).catch(error => {
      if (!controller.signal.aborted) setReadError(message(error));
    });
    return () => controller.abort();
  }, [base, selected, refresh, request]);

  function goToPage(next: number[]) {
    setCursors(next); setPage(null); setListError('');
    setSelected(''); setMail(null); setReadError(''); heading.current?.focus();
  }

  function open(id: string) {
    if (id === selected) { reader.current?.focus(); return; }
    setMail(null); setReadError(''); setSelected(id);
  }

  function closeMessage() {
    setSelected(''); setMail(null); setReadError('');
    requestAnimationFrame(() => (document.getElementById(`inbox-message-${selected}`) ?? heading.current)?.focus());
  }

  async function copyAddress() {
    if (!page?.address) return;
    try { await copyText(page.address); setCopyNotice('Address copied.'); }
    catch { setCopyNotice('Select the address to copy it.'); }
  }

  return { page, before, cursors, listError, selected, mail, readError, copyNotice, heading, reader, open, closeMessage, copyAddress, goToPage, refresh: () => setRefresh(value => value + 1) };
}

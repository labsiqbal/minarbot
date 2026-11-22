import { useCachedQuery } from './hooks/useCachedQuery';
import { useEffect, useRef, useState } from 'react';
import { Info } from 'lucide-react';
import { VoiceSettings } from './SpeechPlayback';
import { WebSearchSettings } from './WebSearchSettings';
import { LibraryDocsSettings } from './LibraryDocsSettings';
import { Alert, Card, IconButton, PageHeader, Row, Section, Switch } from './ui/kit';

export function GeneralSettings({ request, notificationsMuted, notificationError, onNotificationsChange }: { request: <T>(path: string, data?: unknown) => Promise<T>; notificationsMuted: boolean; notificationError: string; onNotificationsChange: (enabled: boolean) => void }) {
  const heading = useRef<HTMLHeadingElement>(null);
  useEffect(() => { heading.current?.focus(); }, []);
  const [permission, setPermission] = useState(() => typeof Notification === 'undefined' ? 'unsupported' : Notification.permission);
  const [permissionError, setPermissionError] = useState('');
  async function changeNotifications(enabled: boolean) {
    setPermissionError('');
    if (enabled && Notification.permission !== 'granted') {
      try {
        const next = await Notification.requestPermission();
        setPermission(next);
        if (next !== 'granted') return;
      } catch {
        setPermissionError('Notification permission could not be requested. Check browser settings.');
        return;
      }
    }
    onNotificationsChange(enabled);
  }
  const path = '/api/conversations/settings';
  const { data, setData, error } = useCachedQuery<{ autoArchive: boolean }>(request, path);
  const [chatError, setChatError] = useState(''), [chatBusy, setChatBusy] = useState(false);
  async function changeArchive(value: boolean) {
    setChatBusy(true); setChatError('');
    try { setData(await request<{ autoArchive: boolean }>(path, { id: crypto.randomUUID(), autoArchive: value })); }
    catch { setChatError('Conversation settings could not be saved. Reopen General to check the current value.'); }
    finally { setChatBusy(false); }
  }
  return <div className="k-page bs-root">
    <PageHeader headingRef={heading} title="General" />
    <Section title="Notifications"><Card><Row title="Agent and workflow notifications" trailing={<Switch label="Agent and workflow notifications" checked={permission === 'granted' && !notificationsMuted} disabled={permission === 'unsupported' || permission === 'denied'} onChange={enabled => void changeNotifications(enabled)} />} /></Card>{(notificationError || permissionError) && <Alert tone="error">{notificationError || permissionError}</Alert>}{permission === 'denied' && <Alert>Notifications are blocked. Change site permissions in browser settings.</Alert>}</Section>
    <VoiceSettings />
    <Section title="Conversations"><Card><Row title="Auto-archive agent deliveries" trailing={<><span className="settings-fact">7 days</span><IconButton label="After work settles; history stays saved." aria-label="About auto-archive"><Info size={14} aria-hidden /></IconButton><Switch label="Auto-archive agent deliveries" checked={data?.autoArchive ?? true} disabled={data === null || chatBusy} onChange={value => void changeArchive(value)} /></>} /></Card>{(chatError || !!error) && <Alert tone="error">{chatError || 'Conversation settings could not be loaded. Reopen General to retry.'}</Alert>}</Section>
    <LibraryDocsSettings request={request} />
    <WebSearchSettings request={request} />
  </div>;
}

import { useState } from 'react';
import { Check, Plug, Plus, Send } from 'lucide-react';
import { AgentForm, type Agent, type FirstRunConnection } from './AgentProfileDialog';
import { ProviderForm, type ProviderAccount } from './ProvidersPanel';
import { DEVICE_BINDING, engineLoginsPath, loginBinding, type EngineLogin, type RuntimeChoice } from './EnginesPanel';
import { Alert, Button, PageHeader } from './ui/kit';
import { ComputerSetup } from './ComputerSetup';
import './first-run.css';
import { useSetupData } from './hooks/useSetupData';

const focusHeading = (heading: HTMLHeadingElement | null) => heading?.focus();
const steps = [
  { title: 'Connect an AI', icon: Plug },
  { title: 'Create agent', icon: Plus },
  { title: 'Send first message', icon: Send },
];

export function FirstRun({ request, isClientError, settingsOpen, onOpenEngines, onCreated }: {
  request: <T>(path: string, data?: unknown) => Promise<T>;
  isClientError: (error: unknown) => boolean;
  settingsOpen: boolean;
  onOpenEngines: () => void;
  onCreated: (agent: Agent) => void;
}) {
  const accounts = useSetupData<ProviderAccount[]>(request, '/api/provider-accounts', !settingsOpen);
  const engines = useSetupData<{ choices: RuntimeChoice[] }>(request, '/api/runtimes', !settingsOpen);
  const choices = engines.data?.choices;
  const logins = useSetupData<EngineLogin[]>(request, engineLoginsPath, !settingsOpen);
  const [adding, setAdding] = useState(false);
  // A provider saved here is an explicit choice, so it outranks a detected sign-in.
  const [savedAccount, setSavedAccount] = useState('');
  const [selectedValue, setSelectedValue] = useState('');
  const error = accounts.error || engines.error || logins.error;
  const ready = choices?.filter(choice => choice.available && choice.enabled) ?? [];
  const signedIn = ready.filter(choice => choice.signedIn === true);
  const codexReady = ready.some(choice => choice.runtime === 'codex');
  const devices = (['codex', 'claude'] as const).flatMap<FirstRunConnection>(runtime => {
    const choice = signedIn.find(choice => choice.runtime === runtime);
    return choice ? [{ value: `device:${runtime}`, label: choice.label, runtime, defaults: { runtime, account: DEVICE_BINDING, model: 'default' } }] : [];
  });
  const providers = accounts.data?.filter(account => account.configured).map<FirstRunConnection>(account => {
    const runtime = account.provider === 'openai-compatible' && codexReady ? 'codex' : 'api';
    return { value: `account:${account.id}`, label: account.name, runtime, defaults: { runtime, account: account.id, model: account.defaultModel } };
  }) ?? [];
  const hosts = signedIn.filter(({ runtime }) => runtime !== 'codex' && runtime !== 'claude').map<FirstRunConnection>(({ runtime, label, models }) => ({
    value: `host:${runtime}`, label, runtime, defaults: { runtime, account: '', model: models[0]?.id ?? 'default' },
  }));
  const readyLogins = logins.data?.filter(login => login.signedIn && ready.some(choice => choice.runtime === login.runtime)) ?? [];
  const otherConnections = [...devices, ...providers, ...hosts];
  const engineLogins = readyLogins.map<FirstRunConnection>(login => {
    const { name, runtime } = login;
    const value = loginBinding(login);
    const ambiguous = otherConnections.some(item => item.label === name) || readyLogins.some(other => other.id !== login.id && other.name === name);
    const label = ambiguous ? `${name} (${ready.find(choice => choice.runtime === runtime)?.label})` : name;
    return { value, label, runtime, defaults: { runtime, account: value, model: 'default' } };
  });
  const connections = [...devices, ...engineLogins, ...providers, ...hosts];
  const account = providers.find(item => item.defaults.account === savedAccount) ?? providers.find(item => item.runtime === 'codex') ?? providers[0];
  const automatic = account && (savedAccount || !devices.length) ? account : devices[0] ?? engineLogins[0] ?? hosts[0];
  const selection = connections.find(item => item.value === selectedValue) ?? automatic;
  const defaults = selection?.defaults;
  const activeStep = defaults && !adding ? 1 : 0;

  return <main className="chat page-view bs-root"><div className="page-scroll">
    <div className="first-run">
      <PageHeader headingRef={focusHeading} title="Set up Minarbot" />
      {error && <Alert tone="error" actions={<Button size="sm" onClick={() => { accounts.reload(); engines.reload(); logins.reload(); }}>Retry</Button>}>Setup status could not be loaded.</Alert>}
      {!settingsOpen && <div className="first-run__computer"><ComputerSetup request={request} onboarding /></div>}
      <ol className="first-run__steps" aria-label="Setup progress">
        {steps.map(({ title, icon: Icon }, index) => {
          const done = index < activeStep;
          const active = index === activeStep;
          return <li key={title} aria-current={active ? 'step' : undefined} className={done ? 'is-done' : active ? 'is-active' : undefined}>
            <div className="first-run__step">
              <span className="first-run__icon" aria-hidden>{done ? <Check size={15} /> : <Icon size={15} />}</span>
              <strong>{title}</strong>
              {done && <span className="first-run__status">{selection?.label}</span>}
              {done && !selection?.value.startsWith('account:') && <Button onClick={() => setAdding(true)}>Use an API key</Button>}
            </div>
            {active && <div className="first-run__form">
              {index === 0 && (accounts.data || accounts.error) && <div className="k-stack">
                {!defaults && <>
                  <p className="k-field__hint">Sign in to Codex or Claude Code on this machine, then check again. No API key needed.</p>
                  <Button onClick={() => { engines.reload(); logins.reload(); }} busy={engines.busy}>Check again</Button>
                </>}
                <Button onClick={() => { setAdding(false); onOpenEngines(); }}>Set up a separate engine login</Button>
                <p className="k-field__hint">Or use an API key from an AI provider.</p>
                <div onFocusCapture={() => setAdding(true)}><ProviderForm request={request} onSaved={saved => {
                  accounts.setData(current => [...current ?? [], saved]);
                  setSavedAccount(saved.id);
                  setSelectedValue(`account:${saved.id}`);
                  setAdding(false);
                }} /></div>
                {defaults && <Button variant="ghost" onClick={() => setAdding(false)}>Use {selection?.label}</Button>}
              </div>}
              {index === 1 && ((choices || !engines.busy) && selection
                ? <AgentForm request={request} isClientError={isClientError} model={selection.defaults.model} firstRun={selection.defaults}
                  connections={connections} connection={selection.value} onConnectionChange={setSelectedValue} onCreated={onCreated} />
                : <span className="k-field__hint" role="status">Checking engines…</span>)}
            </div>}
          </li>;
        })}
      </ol>
    </div>
  </div></main>;
}

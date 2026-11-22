import { useRef, useState } from 'react';
import { useSetupData } from './hooks/useSetupData';
import { Alert, Badge, Button, Card, PageHeader, Row, Section, SkeletonRows, Switch, useConfirm } from './ui/kit';

type Request = <T>(path: string, data?: unknown) => Promise<T>;
type Item = { id: string; name: string };
type Status = { version: string; installed: boolean; available: boolean; computerReady: boolean; agents: string[]; deepProjects: string[] };
const endpoint = '/api/settings/craft-anything';
const focusHeading = (heading: HTMLHeadingElement | null) => heading?.focus();

export function CraftAnythingSettings({ request, agents, projects }: { request: Request; agents: Item[]; projects: Item[] }) {
  const { data, error, reload, setData } = useSetupData<Status>(request, endpoint);
  const [pending, setPending] = useState('');
  const [failure, setFailure] = useState('');
  const flight = useRef(false);
  const confirm = useConfirm();

  async function save(action: string, body = {}) {
    if (flight.current) return;
    flight.current = true;
    setPending(action);
    setFailure('');
    try { setData(await request<Status>(`${endpoint}/${action}`, body)); }
    catch (cause) { setFailure(cause instanceof Error ? cause.message : 'Craft Anything request failed.'); }
    finally { flight.current = false; setPending(''); }
  }

  async function setDeep(project: Item, enabled: boolean) {
    if (enabled && !await confirm({
      title: `Allow Deep mode for ${project.name}?`,
      description: 'Deep mode can inspect source and decompile files. Continue only if you own or have permission to inspect the targets in this project.',
      confirm: 'Confirm permission',
    })) return;
    await save('deep', { project: project.id, confirmedOwnership: enabled });
  }

  return <div className="k-page bs-root">
    <PageHeader headingRef={focusHeading} title="Craft Anything" />
    {error && <Alert tone="error" actions={<Button size="sm" onClick={reload}>Reload</Button>}>Craft Anything status could not be loaded.</Alert>}
    {failure && <Alert tone="error">{failure}</Alert>}
    {!data ? !error && <SkeletonRows count={2} /> : <>
      <Card>
        <Row title={`Version ${data.version}`} meta={<a href="https://github.com/storytold/craft-anything" target="_blank" rel="noreferrer">Third-party source · MIT or Apache-2.0</a>}
          trailing={<>
            <span role="status"><Badge tone={data.installed ? 'success' : undefined}>{data.installed ? 'Installed' : 'Not installed'}</Badge></span>
            {data.installed ? <Button size="sm" disabled={!!pending} busy={pending === 'remove'} onClick={() => void save('remove')}>Remove</Button>
              : <Button size="sm" variant="primary" disabled={!!pending || !data.available} busy={pending === 'install'} onClick={() => void save('install')}>Install</Button>}
          </>} />
      </Card>
      {!data.available && <Alert>Install requires Linux with Bubblewrap, GPG and tar.</Alert>}
      {data.available && !data.computerReady && <Alert><a href="/settings/computer">Set up Computer</a> to use Craft Anything.</Alert>}
      {data.installed && <>
        <Card><Row title="fal.ai key" meta={<code>craft-anything-fal</code>} trailing={<a href="/settings/vault">Manage in Vault</a>} /></Card>
        <Section title="Agent access">
          {agents.length ? <Card>{agents.map(agent => <Row key={agent.id} title={agent.name} trailing={
            <Switch label={`Craft Anything access for ${agent.name}`} checked={data.agents.includes(agent.id)} disabled={!!pending}
              onChange={enabled => void save('grant', { agent: agent.id, enabled })} />} />)}</Card> : <p className="empty">Create an agent to grant access.</p>}
        </Section>
        <Section title="Deep mode">
          {projects.length ? <Card>{projects.map(project => <Row key={project.id} title={project.name}
            meta={data.deepProjects.includes(project.id) ? 'Deep mode allowed' : 'Clean room'}
            trailing={<Switch label={`Deep mode for ${project.name}`} checked={data.deepProjects.includes(project.id)}
              disabled={!!pending} onChange={enabled => void setDeep(project, enabled)} />} />)}</Card> : <p className="empty">Create a project to confirm Deep mode access.</p>}
        </Section>
      </>}
    </>}
  </div>;
}

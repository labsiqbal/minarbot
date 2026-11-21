import { useCachedData, useQueryPolling } from './hooks/useCachedQuery';
import { queryCache } from './hooks/queryCache';
import { getNonce } from 'get-nonce';
import { SkillPacksPanel } from './SkillPacksPanel';
import { useEffect, useRef, useState } from 'react';
import { restoreSkillCommand, type SkillCommand } from '../shared/skill-command';
import { parseSkillFiles, type SkillFile } from '../shared/skill-files';
import { SkillFolderImport, downloadSkillFolder } from './SkillFolderImport';
import { BookOpen, ChevronDown, ChevronLeft, ChevronRight, Ellipsis, FileText, Plus } from 'lucide-react';
import { ActionMenu, Alert, Badge, Button, Card, Collapsible, Dialog, EmptyState, Field, IconButton, Input, PageHeader, Row, Section, Select, SkeletonRows, Switch, TabPanel, Tabs, Textarea, Tooltip, useConfirm, RecoveryAlert } from './ui/kit';
type Skill = { id: string; name: string; description: string; content: string; revision: number; latest: number; files?: SkillFile[] };
type Installed = { pack?: string | null; removed: number; skill: string; name: string; version: number; enabled: number; revision: number; latest: number };
const storage = 'minarbot:skill-command';
export function SkillsPanel({ agents, request, onClose }: { agents: { id: string; name: string }[]; request: <T>(path: string, body?: unknown) => Promise<T>; onClose: () => void }) {
  const [tab, setTab] = useState('library');
  const importControl = useRef<HTMLSpanElement>(null);
  const [owner, setOwner] = useState(agents[0]?.id ?? '');
  const [recovery] = useState(() => { try { return { command: restoreSkillCommand(sessionStorage.getItem(storage)), invalid: false }; } catch { return { command: null, invalid: true }; } });
  const [pending, setPending] = useState(recovery.command), [invalid, setInvalid] = useState(recovery.invalid);
  const [installCursors, setInstallCursors] = useState(['']), installAfter = installCursors.at(-1)!;
  const [binding, setBinding] = useState<Installed | null | undefined>(undefined);
  const [baseline, setBaseline] = useState('');
  const [cursors, setCursors] = useState(['']), [refresh, setRefresh] = useState(0), [error, setError] = useState(''), [busy, setBusy] = useState(false);
  const [editor, setEditor] = useState<Skill | null>(null), [version, setVersion] = useState(1);
  const [importBusy, setImportBusy] = useState(false), [loadError, setLoadError] = useState('');
  const navigation = useRef(0), inFlight = useRef(false), heading = useRef<HTMLHeadingElement>(null);
  const dirty = !!editor && JSON.stringify(editor) !== baseline;
  const confirm = useConfirm();
  const canReplace = async () => !dirty || confirm({ title: 'Discard skill changes?', description: 'Unsaved changes will be lost.', confirm: 'Discard changes', danger: true });
  const locked = busy || importBusy || !!pending || invalid, after = cursors.at(-1)!;
  useEffect(() => { heading.current?.focus(); return () => { navigation.current++; }; }, []);
  useEffect(() => { heading.current?.focus(); }, [after]);
  const path = `/api/skills${after ? `?after=${after}` : ''}`;
  const installPath = `/api/agents/${owner}/skills${installAfter ? `?after=${installAfter}` : ''}`;
  const { data: page } = useCachedData<{ skills: Skill[]; next_after: string | null }>(path);
  const { data: installs } = useCachedData<{ items: Installed[]; next_after: string | null }>(installPath);
  const installed = owner ? installs?.items ?? null : [], installNext = installs?.next_after ?? null;
  useQueryPolling(async signal => {
    try {
      await Promise.all([queryCache.refresh(path, () => request<NonNullable<typeof page>>(path)), owner ? queryCache.refresh(installPath, () => request<NonNullable<typeof installs>>(installPath)) : null]);
      if (!signal.aborted) setLoadError('');
    } catch (error) { if (!signal.aborted) setLoadError(error instanceof Error ? error.message : 'Skills could not load.'); }
  }, { interval: 15000 }, [owner, after, installAfter, refresh, request]);
  useEffect(() => {
    let stopped = false; setBinding(undefined);
    if (owner && editor?.id) void request<Installed | null>(`/api/agents/${owner}/skills/${editor.id}`).then(row => { if (!stopped) setBinding(row); }).catch(error => { if (!stopped) setError(error.message); });
    return () => { stopped = true; };
  }, [owner, editor?.id, refresh]);
  async function open(id: string, revision?: number) {
    if (locked || !await canReplace()) return;
    const current = ++navigation.current; setBusy(true); setError('');
    try { const skill = await request<Skill>(`/api/skills/${id}${revision ? `?version=${revision}` : ''}`); if (current === navigation.current) { setEditor(skill); setBaseline(JSON.stringify(skill)); setVersion(skill.revision); } }
    catch (error) { if (current === navigation.current) setError(error instanceof Error ? error.message : 'Skill could not load.'); }
    finally { if (current === navigation.current) setBusy(false); }
  }
  async function execute(command: SkillCommand) {
    if (inFlight.current || busy || invalid) return;
    const current = navigation.current; setError('');
    try {
      restoreSkillCommand(JSON.stringify(command)); sessionStorage.setItem(storage, JSON.stringify(command)); setPending(command); inFlight.current = true; setBusy(true);
      await request(command.path, command.body);
      if (current !== navigation.current) return;
      if (sessionStorage.getItem(storage) === JSON.stringify(command)) sessionStorage.removeItem(storage);
      setPending(null); setRefresh(value => value + 1); if (!command.path.startsWith('/api/agents/')) { setEditor(null); heading.current?.focus(); }
    } catch (error) { if (current === navigation.current) setError(error instanceof Error ? error.message : 'Changes are not confirmed yet. Try again.'); }
    finally { inFlight.current = false; if (current === navigation.current) setBusy(false); }
  }
  async function discard() {
    if (busy || !await confirm({ title: 'Discard saved request?', description: 'Changes may already have succeeded. The list reloads before the next change.', confirm: 'Discard request' })) return;
    try { sessionStorage.removeItem(storage); setPending(null); setInvalid(false); setRefresh(value => value + 1); }
    catch { setError('Browser storage could not be updated.'); }
  }
  const ownerName = agents.find(agent => agent.id === owner)?.name ?? '';
  const startNew = async () => { if (!await canReplace()) return; setBaseline(''); setEditor({ id: '', name: '', description: '', content: '', revision: 0, latest: 0 }); setVersion(1); };
  const importFiles = async (files: SkillFile[]) => { if (!await canReplace()) return; const parsed = parseSkillFiles(files); setEditor({ id: editor?.id ?? '', revision: editor?.revision ?? 0, latest: editor?.latest ?? 0, ...parsed }); setVersion(editor?.revision || 1); };
  const save = () => { if (!editor || locked) return; void execute({ path: editor.id ? `/api/skills/${editor.id}` : '/api/skills', body: { id: crypto.randomUUID(), revision: editor.latest, ...(editor.files ? { files: editor.files } : { name: editor.name, description: editor.description, content: editor.content }) } }); };
  const install = async () => {
    if (!editor || !await confirm({ title: `Install version ${editor.revision} on ${ownerName}?`, description: 'Unsaved text changes are excluded from installation. Running executions stay unchanged.', confirm: 'Install' })) return;
    void execute({ path: `/api/agents/${owner}/skills/${editor.id}`, body: { id: crypto.randomUUID(), revision: binding?.revision ?? 0, version: editor.revision, enabled: true } });
  };
  const pager = (back: boolean, next: boolean, onBack: () => void, onNext: () => void, label: string) => (back || next) && <div className="k-pager">
    <IconButton label={`${label} previous`} disabled={locked || !back} onClick={onBack}><ChevronLeft size={16} aria-hidden /></IconButton>
    <IconButton label={`${label} next`} disabled={locked || !next} onClick={onNext}><ChevronRight size={16} aria-hidden /></IconButton>
  </div>;
  const recoveryNotice = <>
    {invalid && <Alert tone="error" title="Recovery unavailable" actions={<Button size="sm" disabled={busy} onClick={() => void discard()}>Discard request</Button>} />}
    {pending && <RecoveryAlert inFlight={busy} tone="warn" title="Not confirmed" actions={<><Button size="sm" variant="primary" busy={busy} onClick={() => void execute(pending)}>Try again</Button><Button size="sm" disabled={busy} onClick={() => void discard()}>Discard request</Button></>} />}
  </>;
  return <div className="k-page bs-root bs-set3-skills">
    <style nonce={getNonce()}>{`.bs-root.bs-set3-skills .k-row__title { white-space: normal; overflow-wrap: anywhere; } .bs-root.bs-set3-skills .k-row__meta { overflow-wrap: anywhere; } .bs-root.bs-set3-skills .k-tabs__panel[hidden] { display: none; }`}</style>
    <PageHeader headingRef={heading} title="Skills"
      actions={<>
        {tab !== 'packs' && <><span hidden ref={importControl}><SkillFolderImport compact disabled={locked} onBusy={setImportBusy} onError={setError} onImport={files => void importFiles(files)} /></span>
        <ActionMenu label="Import" trigger={<Button disabled={locked}>Import<ChevronDown size={14} aria-hidden /></Button>} items={[
          { label: 'Folder', onSelect: () => importControl.current?.querySelectorAll('button')[0]?.click() },
          { label: 'File', onSelect: () => importControl.current?.querySelectorAll('button')[1]?.click() },
        ]} /></>}
        {tab !== 'packs' && <Button variant="primary" icon={<Plus size={14} aria-hidden />} disabled={locked} onClick={() => void startNew()}>Create skill</Button>}
      </>} />
    {error && <Alert tone="error">{error}</Alert>}
    {recoveryNotice}

    <Tabs label="Skills" value={tab} onChange={value => { if (!busy && !importBusy && (value !== 'packs' || !locked)) setTab(value); }} tabs={[{ value: 'library', label: 'Library' }, { value: 'installed', label: 'Installed' }, { value: 'packs', label: 'Packs' }]}>
    <TabPanel value="installed">
    <Section id="skills-installed" title="Agent" actions={agents.length > 0 && <Select size="sm" label="Agent" value={owner} disabled={locked} onChange={value => { navigation.current++; setOwner(value); setInstallCursors(['']); }} options={agents.map(agent => ({ value: agent.id, label: agent.name }))} />}>
      {!owner ? <EmptyState icon={<BookOpen size={18} />} title="No agents yet" />
        : loadError ? <Alert tone="error" actions={<Button size="sm" disabled={busy} onClick={() => setRefresh(value => value + 1)}>Try again</Button>}>{loadError}</Alert>
        : installed === null ? <><span className="sr-only" role="status">Loading installed skills...</span><SkeletonRows count={2} /></>
        : installed.length === 0 ? <EmptyState icon={<BookOpen size={18} />} title="No installed skills" />
        : <Card>{installed.map(row => <Row key={row.skill} title={row.name} disabled={locked} onClick={() => void open(row.skill, row.version)} leading={<BookOpen size={16} aria-hidden />}
            meta={<><span>Version {row.version}</span>{row.latest > row.version && <Badge tone="warn">Version {row.latest} available</Badge>}{row.pack && <Badge>Pack</Badge>}{!row.enabled && <Badge>Inactive</Badge>}</>}
            trailing={<>
              <Switch checked={!!row.enabled} disabled={locked || !!row.pack} label={`${row.enabled ? 'Disable' : 'Enable'} ${row.name}`} onChange={() => void execute({ path: `/api/agents/${owner}/skills/${row.skill}`, body: { id: crypto.randomUUID(), revision: row.revision, version: row.version, enabled: !row.enabled } })} />
              <ActionMenu label={`Menu ${row.name}`} trigger={<IconButton label={`Menu ${row.name}`} tooltip="More actions" disabled={locked}><Ellipsis size={16} aria-hidden /></IconButton>} items={[
                { label: `Review version ${row.version}`, onSelect: () => void open(row.skill, row.version) },
                'separator',
                { label: row.pack ? 'Managed by Skill packs' : 'Remove from agent', danger: !row.pack, disabled: !!row.pack, onSelect: async () => { if (await confirm({ title: `Remove ${row.name} from ${ownerName}?`, description: 'Library revisions remain available. Executions already running stay unchanged.', confirm: 'Remove', danger: true })) void execute({ path: `/api/agents/${owner}/skills/${row.skill}`, body: { id: crypto.randomUUID(), revision: row.revision, version: row.version, enabled: false, remove: true } }); } },
              ]} />
            </>} />)}</Card>}
      {pager(installCursors.length > 1, !!installNext, () => setInstallCursors(value => value.slice(0, -1)), () => setInstallCursors(value => [...value, installNext!]), 'Installation')}
    </Section>

    </TabPanel>
    <TabPanel value="library">
    <div id="skills-library" className="k-stack">
      {loadError ? <Alert tone="error" actions={<Button size="sm" disabled={busy} onClick={() => setRefresh(value => value + 1)}>Try again</Button>}>{loadError}</Alert>
        : !page ? <><span className="sr-only" role="status">Loading skills...</span><SkeletonRows count={3} /></>
        : page.skills.length === 0 ? <EmptyState icon={<FileText size={18} />} title="No skills" />
        : <Card>{page.skills.map(skill => <Row key={skill.id} title={skill.name} meta={`v${skill.revision}`} leading={<FileText size={16} aria-hidden />} disabled={locked} onClick={() => void open(skill.id)} trailing={<ActionMenu label={`Menu ${skill.name}`} trigger={<IconButton label={`Menu ${skill.name}`} tooltip="More actions" disabled={locked}><Ellipsis size={16} aria-hidden /></IconButton>} items={[{ label: 'Edit skill', onSelect: () => void open(skill.id) }]} />} />)}</Card>}
      {pager(cursors.length > 1, !!page?.next_after, () => setCursors(value => value.slice(0, -1)), () => setCursors(value => [...value, page!.next_after!]), 'Skill')}
    </div>
    </TabPanel>
    <TabPanel value="packs"><SkillPacksPanel embedded headingRef={heading} agents={agents} initialOwner={owner} request={request} onClose={() => { setTab('library'); setRefresh(value => value + 1); }} /></TabPanel>
    </Tabs>

    <Dialog open={!!editor} size="lg" onOpenChange={async next => { if (!next && await canReplace()) setEditor(null); }}
      title={editor?.id ? editor.name : editor?.files ? `Import ${editor.name}` : 'New skill'}
      description={editor?.id ? <Tooltip content="Agents keep installed version"><span tabIndex={0}>v{editor.revision} of {editor.latest}</span></Tooltip> : undefined}
      footer={<>
        {editor?.id && <span className="k-dialog__foot-note"><SkillFolderImport compact disabled={locked} onBusy={setImportBusy} onError={setError} onImport={files => void importFiles(files)} /></span>}
        <Button disabled={busy} onClick={async () => { if (await canReplace()) setEditor(null); }}>Close</Button>
        <Button type="submit" form="skill-editor" variant="primary" busy={busy} disabled={locked && !busy}>Save version</Button>
      </>}>
      {editor && <form id="skill-editor" className="k-page" style={{ gap: 18 }} onSubmit={event => { event.preventDefault(); save(); }}>
        {error && <Alert tone="error">{error}</Alert>}{recoveryNotice}
        <fieldset disabled={locked} className="k-fieldset">
          <Field label="Name"><Input required readOnly={!!editor.files} maxLength={80} value={editor.name} onChange={event => setEditor({ ...editor, name: event.target.value })} placeholder="For example: meeting-summary" /></Field>
          <Field label="When to use"><Textarea required readOnly={!!editor.files} maxLength={1024} rows={2} style={{ minHeight: 60 }} value={editor.description} onChange={event => setEditor({ ...editor, description: event.target.value })} /></Field>
          <Field label="Instructions"><Textarea required readOnly={!!editor.files} maxLength={16000} rows={10} style={{ minHeight: 200 }} value={editor.content} onChange={event => setEditor({ ...editor, content: event.target.value })} /></Field>
        </fieldset>
        {editor.files && <Section title={`${editor.files.length} files from folder`} actions={<>
          <Button size="sm" variant="ghost" onClick={() => downloadSkillFolder(editor.files!)}>Export JSON</Button>
          <Button size="sm" variant="ghost" onClick={() => { try { downloadSkillFolder(editor.files!, true); } catch (error) { setError(error instanceof Error ? error.message : 'ZIP export failed. Check skill files.'); } }}>Export ZIP</Button>
        </>}>

          <Card className="k-file-list">{editor.files.map(file => <div key={file.path} className="k-file"><Collapsible title={<span className="k-file__path">{file.path}</span>}>{file.encoding ? <p className="k-file__note">Binary asset. Export ZIP to get the original file.</p> : <pre className="k-file__content" tabIndex={0} role="region" aria-label={`Contents of ${file.path}`}>{file.content}</pre>}</Collapsible></div>)}</Card>
        </Section>}
        {editor.id && <Section title="Version">
          <div className="k-version">
            <Field label="Version"><div className="k-inline"><Input type="number" min={1} max={editor.latest} step={1} value={version} onChange={event => setVersion(Number(event.target.value))} style={{ width: 96 }} /><Button disabled={!Number.isSafeInteger(version) || version < 1 || version > editor.latest || locked} onClick={() => void open(editor.id, version)}>Load</Button></div></Field>
            <div className="k-version__install">
              <Select label="Install on agent" size="sm" value={owner} disabled={locked} onChange={value => { navigation.current++; setOwner(value); setInstallCursors(['']); }} options={agents.map(agent => ({ value: agent.id, label: agent.name }))} />
              {binding?.removed === 1 && <Badge>Removed</Badge>}
              <Button variant="brand" disabled={!owner || binding === undefined || (!!binding?.pack && !binding.removed) || locked} onClick={() => void install()}>Install v{editor.revision}</Button>
            </div>
          </div>
        </Section>}
      </form>}
    </Dialog>
  </div>;
}

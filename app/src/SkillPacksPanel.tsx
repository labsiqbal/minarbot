import { useCachedData, useQueryPolling } from './hooks/useCachedQuery';
import { queryCache } from './hooks/queryCache';
import { useEffect, useRef, useState, type RefObject } from 'react';
import { restoreSkillCommand, type SkillCommand } from '../shared/skill-command';
import { ArrowLeft, ChevronLeft, ChevronRight, Ellipsis, FileText, Package, Plus } from 'lucide-react';
import { ActionMenu, Alert, Badge, Button, Card, CodeBlock, Collapsible, Dialog, EmptyState, Field, IconButton, Input, PageHeader, Row, Section, Select, SkeletonRows, Switch, Tooltip, useConfirm, RecoveryAlert } from './ui/kit';
type Member = { skill: string; version: number; name: string; description: string; content: string };
type Pack = { id: string; name: string; revision: number; latest: number; members: Member[] };
type Binding = { revision: number; version: number; enabled: number; removed: number };
type Page = { packs: Pack[]; next_after: string | null };
type Library = { skills: { id: string; name: string; revision: number }[]; next_after: string | null };
const storage = 'minarbot:pack-command';
export function SkillPacksPanel({ agents, initialOwner, request, onClose, embedded = false, headingRef }: { headingRef?: RefObject<HTMLHeadingElement | null>; embedded?: boolean; agents: { id: string; name: string }[]; initialOwner: string; request: <T>(path: string, body?: unknown) => Promise<T>; onClose: () => void }) {
  const [owner, setOwner] = useState(initialOwner);
  const [cursors, setCursors] = useState(['']), [skillCursors, setSkillCursors] = useState(['']), [refresh, setRefresh] = useState(0);
  const [editor, setEditor] = useState<Pack | null>(null), [baseline, setBaseline] = useState(''), [binding, setBinding] = useState<Binding | null | undefined>(undefined);
  const [version, setVersion] = useState(1), [skill, setSkill] = useState(''), [skillVersion, setSkillVersion] = useState(1);
  const [recovery] = useState(() => { try { const command = restoreSkillCommand(sessionStorage.getItem(storage)); if (command && !command.path.includes('/skill-packs')) throw Error(); return { command, invalid: false }; } catch { return { command: null, invalid: true }; } });
  const [pending, setPending] = useState(recovery.command), [invalid, setInvalid] = useState(recovery.invalid), [busy, setBusy] = useState(false), [error, setError] = useState('');
  const localHeading = useRef<HTMLHeadingElement>(null), heading = headingRef ?? localHeading, generation = useRef(0), inFlight = useRef(false);
  const after = cursors.at(-1)!, skillAfter = skillCursors.at(-1)!, locked = busy || !!pending || invalid;
  const confirm = useConfirm();
  const replace = async () => !editor || JSON.stringify(editor) === baseline || confirm({ title: 'Discard unsaved pack changes?', confirm: 'Discard changes', danger: true });
  useEffect(() => { if (!embedded) heading.current?.focus(); return () => { generation.current++; }; }, []);
  const [loadError, setLoadError] = useState('');
  const path = `/api/skill-packs${after ? `?after=${after}` : ''}`;
  const libraryPath = `/api/skills${skillAfter ? `?after=${skillAfter}` : ''}`;
  const { data: page } = useCachedData<Page>(path);
  const { data: library } = useCachedData<Library>(libraryPath);
  useQueryPolling(async signal => {
    try {
      await Promise.all([queryCache.refresh(path, () => request<Page>(path)), queryCache.refresh(libraryPath, () => request<Library>(libraryPath))]);
      if (!signal.aborted) setLoadError('');
    } catch (error) { if (!signal.aborted) setLoadError(error instanceof Error ? error.message : 'Packs could not load.'); }
  }, { interval: 15000 }, [after, skillAfter, refresh, request]);
  useEffect(() => {
    let stopped = false; setBinding(undefined);
    if (editor?.id && owner) void request<Binding | null>(`/api/agents/${owner}/skill-packs/${editor.id}`).then(row => { if (!stopped) setBinding(row); }).catch(error => { if (!stopped) setError(error.message); });
    return () => { stopped = true; };
  }, [owner, editor?.id, refresh]);
  async function open(id: string, revision?: number) {
    if (locked || !await replace()) return;
    const current = ++generation.current; setBusy(true); setError('');
    try { const row = await request<Pack>(`/api/skill-packs/${id}${revision ? `?version=${revision}` : ''}`); if (current === generation.current) { setEditor(row); setBaseline(JSON.stringify(row)); setVersion(row.revision); heading.current?.focus(); } }
    catch (error) { if (current === generation.current) setError(error instanceof Error ? error.message : 'Pack could not load.'); }
    finally { if (current === generation.current) setBusy(false); }
  }
  async function addMember() {
    if (locked || !editor || !skill || !Number.isSafeInteger(skillVersion) || skillVersion < 1) return;
    const current = ++generation.current; setBusy(true); setError('');
    try { const row = await request<Member & { id: string; revision: number }>(`/api/skills/${skill}?version=${skillVersion}`); if (current === generation.current) setEditor({ ...editor, members: [...editor.members.filter(member => member.skill !== skill), { skill: row.id, version: row.revision, name: row.name, description: row.description, content: row.content }] }); }
    catch (error) { if (current === generation.current) setError(error instanceof Error ? error.message : 'Skill could not load.'); }
    finally { if (current === generation.current) setBusy(false); }
  }
  async function execute(command: SkillCommand) {
    if (busy || inFlight.current || invalid) return;
    const current = generation.current; setError('');
    try {
      restoreSkillCommand(JSON.stringify(command)); sessionStorage.setItem(storage, JSON.stringify(command)); setPending(command); setBusy(true); inFlight.current = true;
      await request(command.path, command.body);
      if (current !== generation.current) return;
      if (sessionStorage.getItem(storage) === JSON.stringify(command)) sessionStorage.removeItem(storage);
      setPending(null); if (!command.path.startsWith('/api/agents/')) setEditor(null); setRefresh(value => value + 1); heading.current?.focus();
    } catch (error) { if (current === generation.current) setError(error instanceof Error ? error.message : 'Changes are not confirmed yet.'); }
    finally { inFlight.current = false; if (current === generation.current) setBusy(false); }
  }
  async function mutate(enabled: boolean, remove = false) {
    if (!editor?.id || !owner || binding === undefined || locked) return;
    const selectedVersion = remove || !enabled ? binding?.version : editor.revision;
    if (!selectedVersion || !await confirm(remove ? { title: 'Remove the pack and all skills it manages?', description: 'The library remains available.', confirm: 'Remove pack', danger: true } : { title: `Apply pack version ${selectedVersion} to this agent?`, description: 'All members change together; unsaved drafts are excluded.', confirm: 'Apply pack' })) return;
    void execute({ path: `/api/agents/${owner}/skill-packs/${editor.id}`, body: { id: crypto.randomUUID(), revision: binding?.revision ?? 0, version: selectedVersion, enabled, ...(remove ? { remove: true } : {}) } });
  }
  const startNew = async () => { if (await replace()) { const row = { id: '', name: '', revision: 0, latest: 0, members: [] }; setEditor(row); setBaseline(JSON.stringify(row)); } };
  const discard = async () => {
    if (await confirm({ title: 'Discard saved request?', description: 'Changes may already have succeeded. Check the list before trying again.', confirm: 'Discard request', danger: true })) try { sessionStorage.removeItem(storage); setPending(null); setInvalid(false); setRefresh(value => value + 1); } catch { setError('Browser storage could not be updated.'); }
  };
  const pager = (back: boolean, next: boolean, disabled: boolean, onBack: () => void, onNext: () => void, label: string) => (back || next) && <div className="k-pager">
    <IconButton label={`Previous ${label}`} disabled={disabled || !back} onClick={onBack}><ChevronLeft size={16} aria-hidden /></IconButton>
    <IconButton label={`Next ${label}`} disabled={disabled || !next} onClick={onNext}><ChevronRight size={16} aria-hidden /></IconButton>
  </div>;
  const recoveryActions = <>
    {pending && <Button size="sm" variant="primary" busy={busy} onClick={() => void execute(pending)}>Try again</Button>}
    <Button size="sm" disabled={busy} onClick={() => void discard()}>Discard request</Button>
  </>;
  const recoveryNotice = <>
    {invalid && <Alert tone="error" title="Recovery unavailable" actions={recoveryActions} />}
    {pending && <RecoveryAlert inFlight={busy} tone="warn" title="Not confirmed" actions={!invalid && recoveryActions} />}
  </>;
  return <div className="k-page bs-root">
    {!embedded && <PageHeader headingRef={heading} title="Packs"
      actions={<>
        <Button variant="ghost" icon={<ArrowLeft size={14} aria-hidden />} onClick={async () => { if (await replace()) onClose(); }}>Skills</Button>
        <Button variant="primary" icon={<Plus size={14} aria-hidden />} disabled={locked} onClick={() => void startNew()}>Create pack</Button>
      </>} />}
    {embedded && <div className="k-row-actions" style={{ justifyContent: 'flex-end' }}><Button variant="primary" icon={<Plus size={14} aria-hidden />} disabled={locked} onClick={() => void startNew()}>Create pack</Button></div>}
    {(error || loadError) && <Alert tone="error" actions={loadError ? <Button size="sm" onClick={() => setRefresh(value => value + 1)}>Try again</Button> : undefined}>{error || loadError}</Alert>}
    {recoveryNotice}
    <div className="k-stack">
      {!page ? !error && !loadError && <><span className="sr-only" role="status">Loading packs...</span><SkeletonRows /></>
        : !page.packs.length ? <EmptyState icon={<Package size={18} />} title="No packs" />
        : <Card>{page.packs.map(pack => <Row key={pack.id} title={pack.name} leading={<Package size={16} aria-hidden />} meta={<Badge>v{pack.revision}</Badge>} disabled={locked} onClick={() => void open(pack.id)} />)}</Card>}
      {pager(cursors.length > 1, !!page?.next_after, locked, () => setCursors(value => value.slice(0, -1)), () => setCursors(value => [...value, page!.next_after!]), 'packs')}
    </div>
    <Dialog open={!!editor} size="lg" title={editor?.id ? editor.name : 'New pack'}
      description={editor?.id ? <Tooltip content="Agents keep installed version"><span tabIndex={0}>v{editor.revision} of {editor.latest}</span></Tooltip> : undefined}
      onOpenChange={async next => { if (!next && await replace()) setEditor(null); }}
      footer={<>
        <Button disabled={busy} onClick={async () => { if (await replace()) setEditor(null); }}>Close</Button>
        <Button type="submit" form="pack-editor" variant="primary" busy={busy} disabled={locked || !editor?.members.length}>Save version</Button>
      </>}>
      {editor && <>
        {(error || loadError) && <Alert tone="error" actions={loadError ? <Button size="sm" onClick={() => setRefresh(value => value + 1)}>Try again</Button> : undefined}>{error || loadError}</Alert>}
        {recoveryNotice}
        <form id="pack-editor" className="k-stack" onSubmit={event => { event.preventDefault(); void execute({ path: editor.id ? `/api/skill-packs/${editor.id}` : '/api/skill-packs', body: { id: crypto.randomUUID(), revision: editor.latest, name: editor.name, members: editor.members.map(({ skill, version }) => ({ skill, version })) } }); }}>
          <fieldset disabled={locked} className="k-fieldset">
            <Field label="Name"><Input required maxLength={80} value={editor.name} onChange={event => setEditor({ ...editor, name: event.target.value })} /></Field>
            <Section title="Skills">
              {!editor.members.length ? <EmptyState icon={<FileText size={18} />} title="No skills" />
                : <Card>{editor.members.map(member => <Row key={member.skill} title={member.name} meta={<Badge>Version {member.version}</Badge>}
                    trailing={<ActionMenu label={`Menu ${member.name}`} trigger={<IconButton label={`Menu ${member.name}`} tooltip="More actions" disabled={locked}><Ellipsis size={16} aria-hidden /></IconButton>} items={[
                      { label: 'Remove from draft', danger: true, onSelect: () => setEditor({ ...editor, members: editor.members.filter(row => row.skill !== member.skill) }) },
                    ]} />}>
                    <Collapsible title={<>Review instructions for {member.name}</>}><p className="k-field__hint">{member.description}</p><CodeBlock label={`Instructions for ${member.name}`}>{member.content}</CodeBlock></Collapsible>
                  </Row>)}</Card>}
            </Section>
            <div className="k-grid-2">
              <Field label="Skill from library" >
                <Select value={skill} disabled={locked} onChange={value => { setSkill(value); setSkillVersion(library?.skills.find(row => row.id === value)?.revision ?? 1); }} options={[{ value: '', label: 'Choose skill' }, ...(library?.skills.map(row => ({ value: row.id, label: row.name.length > 24 ? row.name.slice(0, 21) + '...' : row.name })) ?? [])]} />
              </Field>
              <Field label="Skill version"><Input type="number" min={1} step={1} value={skillVersion} onChange={event => setSkillVersion(Number(event.target.value))} /></Field>
            </div>
            <div className="k-inline"><Button size="sm" disabled={!skill || editor.members.length >= 64} onClick={() => void addMember()}>Add skill</Button>
              {pager(skillCursors.length > 1, !!library?.next_after, locked, () => { setSkill(''); setSkillCursors(value => value.slice(0, -1)); }, () => { setSkill(''); setSkillCursors(value => [...value, library!.next_after!]); }, 'library page')}
            </div>
            {editor.id && <Section title="Version">
              <Field label="Version"><div className="k-inline"><Input type="number" min={1} max={editor.latest} value={version} onChange={event => setVersion(Number(event.target.value))} style={{ width: 96 }} /><Button size="sm" disabled={!Number.isSafeInteger(version) || version < 1 || version > editor.latest} onClick={() => void open(editor.id, version)}>Load</Button></div></Field>
            </Section>}
          </fieldset>
        </form>
        <Section title={<Tooltip content="Manual installations and other packs will not be overwritten."><span tabIndex={0}>Agent</span></Tooltip>} actions={<Select size="sm" label="Agent" disabled={locked} value={owner} onChange={setOwner} options={agents.map(agent => ({ value: agent.id, label: agent.name.length > 24 ? agent.name.slice(0, 21) + '...' : agent.name }))} />}>

          {!editor.id ? <div><Badge>Save before installing</Badge></div>
            : <Card><Row title={editor.name} meta={binding === undefined ? <span role="status">Loading installation...</span> : !binding || binding.removed ? 'Not installed' : <><Badge>Installed version {binding.version}</Badge><Badge>{binding.enabled ? 'Active' : 'Inactive'}</Badge></>}
                trailing={<>
                  {binding && !binding.removed && <Switch checked={!!binding.enabled} disabled={locked || !owner || binding === undefined || !binding.enabled} label="Disable entire pack" onChange={() => void mutate(false)} />}
                  <ActionMenu label="Pack installation actions" trigger={<IconButton label="Pack installation actions" tooltip="More actions" disabled={locked}><Ellipsis size={16} aria-hidden /></IconButton>} items={[
                    { label: `Install v${editor.revision}`, disabled: !owner || binding === undefined, onSelect: () => void mutate(true) },
                    ...(binding && !binding.removed ? [{ label: 'Remove entire pack', danger: true, onSelect: () => void mutate(false, true) }] : []),
                  ]} />
                </>} /></Card>}
        </Section>
      </>}
    </Dialog>
  </div>;
}

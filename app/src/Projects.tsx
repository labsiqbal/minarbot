import { useCachedData, useQueryPolling } from './hooks/useCachedQuery';
import { queryCache } from './hooks/queryCache';
import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react';
import { ChevronRight, Ellipsis, Folder, FolderOpen, Hash, Plus, X } from 'lucide-react';
import type { LocalProject } from '../shared/local-project';
import { ActionMenu, Alert, Badge, Button, Card, Dialog, EmptyState, Field, IconButton, Input, PageHeader, Row, Section, Select, Switch, TabPanel, Tabs, Textarea, useConfirm, RecoveryAlert } from './ui/kit';
import { AgentAvatar } from './AgentAvatar';
import { SourceControl } from './SourceControl';
import { Collapsible } from 'radix-ui';
import { ConnectedAppsPanel } from './ConnectedAppsPanel';
import { McpPanel } from './McpPanel';
import { ProjectEnvironmentPanel } from './ProjectEnvironmentPanel';
import { AutomationCalendar } from './AutomationCalendar';
import './Projects.css';

type Request = <T>(path: string, body?: unknown) => Promise<T>;
type Agent = { id: string; name: string; avatar_revision?: number };
type Command = { path: string; body: Record<string, unknown> };
const storage = 'minarbot:project-command';
const message = (error: unknown) => error instanceof Error ? error.message : 'Project could not load. Try again.';
export function useProjects(request: Request) {
  const path = '/api/projects';
  const { data } = useCachedData<{ projects: LocalProject[]; root: string; codegraphAvailable?: boolean }>(path);
  const [error,setError] = useState(''), [busy,setBusy] = useState(false), [pending,setPending] = useState<Command | null>(null), [invalid,setInvalid] = useState(false);
  const flight = useRef(false), commandRef = useRef<Command | null>(null), confirm = useConfirm();
  const reload = useCallback(() => queryCache.refresh(path, () => request<NonNullable<typeof data>>(path)), [request]);
  useEffect(() => { try { const raw = sessionStorage.getItem(storage); if(raw) { const value = JSON.parse(raw); if(!value || typeof value.path !== 'string' || !/^\/api\/projects(?:\/[a-f0-9-]{36}\/(?:edit|archive|delete|thread|sections(?:\/[a-f0-9-]{36}\/edit)?))?$/.test(value.path) || !value.body || !/^[a-f0-9-]{36}$/i.test(value.body.id)) throw new Error(); commandRef.current=value;setPending(value); } } catch {setInvalid(true);} },[reload]);
  useQueryPolling(async signal => { try { await reload(); } catch(error) { if (!signal.aborted) setError(message(error)); } }, { interval: 5000 }, [reload]);
  async function execute(path: string, body: Record<string,unknown>): Promise<boolean> {
    if(flight.current || invalid) return false; const command = commandRef.current ?? {path,body};
    if(commandRef.current && (path !== command.path || JSON.stringify(body) !== JSON.stringify(command.body))) {setError('Resolve the pending project change first.');return false;}
    flight.current=true;setBusy(true);setError('');
    try {sessionStorage.setItem(storage,JSON.stringify(command));commandRef.current=command;setPending(command);await request(command.path,command.body);await reload();sessionStorage.removeItem(storage);commandRef.current=null;setPending(null);return true;} catch(error){setError(message(error));return false;} finally{flight.current=false;setBusy(false);}
  }
  const recovery = <>{error && <Alert tone="error" actions={<Button size="sm" onClick={() => void reload().then(()=>setError('')).catch(error=>setError(message(error)))}>Reload projects</Button>}>{error}</Alert>}{(pending || invalid) && <RecoveryAlert inFlight={busy} tone="warn" title={invalid ? 'Saved project request could not be read' : 'Project change is not confirmed'} actions={<>{pending && <Button size="sm" disabled={busy} onClick={()=>void execute(pending.path,pending.body)}>Retry project change</Button>}<Button size="sm" disabled={busy} onClick={async()=>{if(await confirm({title:'Clear project request?',description:'Check project state first. Changes already saved and folders already created remain. Clearing this request does not undo them.',confirm:'Clear request'}))try{sessionStorage.removeItem(storage);commandRef.current=null;setPending(null);setInvalid(false);await reload();}catch(error){setError(message(error));}}}>Clear request</Button></>} />}</>;
  return {projects: data?.projects ?? [], root: data?.root ?? '', codegraphAvailable: data?.codegraphAvailable ?? false, reload,execute,busy,locked:busy || !!pending || invalid,recovery};
}
export type ProjectState = ReturnType<typeof useProjects>;


function Members({agents,members,onChange,disabled}: {agents:Agent[];members:LocalProject['members'];onChange:(members:LocalProject['members'])=>void;disabled?:boolean}) {
  const available=agents.filter(agent=>!members.some(member=>member.agent===agent.id));
  return <div className="k-stack">{members.length > 0 && <Card>{members.map(member=>{const agent=agents.find(agent=>agent.id===member.agent);return <Row key={member.agent} leading={agent && <AgentAvatar agent={agent} size={28}/>} title={agent?.name ?? 'Agent'} trailing={<><Select label={`Access for ${agent?.name ?? 'agent'}`} value={member.write?'write':'read'} onChange={value=>onChange(members.map(item=>item.agent===member.agent?{...item,write:value==='write'}:item))} disabled={disabled} options={[{value:'read',label:'Read only'},{value:'write',label:'Read and write'}]}/><IconButton label={`Remove ${agent?.name ?? 'agent'} from project`} disabled={disabled} onClick={()=>onChange(members.filter(item=>item.agent!==member.agent))}><X size={14} aria-hidden/></IconButton></>}/>;})}</Card>} {!!available.length && <Select label="Add project member" value="" disabled={disabled} onChange={agent=>{if(agent)onChange([...members,{agent,write:false}]);}} options={[{value:'',label:'Add member...'},...available.map(agent=>({value:agent.id,label:agent.name}))]}/>}</div>;
}
function FolderBrowser({request,onChoose,onClose}:{request:Request;onChoose:(path:string)=>void;onClose:()=>void}) {
  const [location,setLocation]=useState(''),[result,setResult]=useState<{path:string;parent:string|null;folders:{name:string;path:string}[]}|null>(null),[error,setError]=useState('');
  useEffect(()=>{let stopped=false;setResult(null);setError('');void request<NonNullable<typeof result>>('/api/project-folders'+(location?'?'+new URLSearchParams({path:location}):'')).then(value=>{if(!stopped)setResult(value);}).catch(error=>{if(!stopped)setError(message(error));});return()=>{stopped=true;};},[location,request]);
  return <section className="project-browser" aria-label="Choose existing folder"><div className="k-row-actions"><Button size="sm" disabled={!result?.parent} onClick={()=>setLocation(result!.parent!)}>Parent folder</Button><Button size="sm" disabled={!result || !result.parent} onClick={()=>{if(result)onChoose(result.path);}}>Use this folder</Button><Button size="sm" onClick={onClose}>Close browser</Button></div>{error && <Alert tone="error">{error}</Alert>}{result ? <><p className="project-path">{result.path}</p><Card>{result.folders.map(folder=><Row key={folder.path} title={folder.name} leading={<Folder size={15} aria-hidden/>} onClick={()=>setLocation(folder.path)}/>)}</Card>{!result.folders.length && <p className="k-field__hint">No available subfolders.</p>}</> : !error && <p role="status">Loading folders...</p>}</section>;
}
export function CreateProject({state,agents,request,onClose,onCreated}:{state:ProjectState;agents:Agent[];request:Request;onClose:()=>void;onCreated:(project:LocalProject)=>void}) {
  const [name,setName]=useState(''),[description,setDescription]=useState(''),[folders,setFolders]=useState<{mode:string;path:string}[]>([]),[members,setMembers]=useState<LocalProject['members']>([]),[browser,setBrowser]=useState<number|null>(null),[error,setError]=useState(''); const confirm=useConfirm();
  async function submit(event:FormEvent){event.preventDefault();if(state.locked)return;const write=members.some(member=>member.write);if(write&&!await confirm({title:'Grant project write access?',description:'Selected agents can create, change, and delete files in these folders while working in this project. Archiving the project keeps folder contents.',confirm:'Grant write access'}))return;const id=crypto.randomUUID();if(await state.execute('/api/projects',{id,name,description,folders:folders.map(folder=>folder.mode==='new'?{mode:'new'}:folder),members,confirmWrite:write})){const result=await request<LocalProject>(`/api/projects/${id}`);onCreated(result);}else setError('Project creation is not confirmed. Check the pending request in the sidebar.');}
  return <Dialog open onOpenChange={open=>{if(!open&&!state.busy)onClose();}} title="New project" size="lg" footer={<><Button disabled={state.busy} onClick={onClose}>Cancel</Button><Button form="new-project" type="submit" variant="primary" busy={state.busy} disabled={state.locked || !name.trim()}>Create project</Button></>}><form id="new-project" className="k-stack" onSubmit={event=>void submit(event)}><Field label="Project name"><Input required maxLength={80} value={name} disabled={state.locked} onChange={event=>setName(event.target.value)}/></Field><Field label="Description" optional><Textarea maxLength={2000} value={description} disabled={state.locked} onChange={event=>setDescription(event.target.value)}/></Field><Section title="Folders">{folders.map((folder,index)=><div className="k-stack" key={index}><Field label={`Folder ${index+1}`}><Select value={folder.mode} disabled={state.locked} onChange={mode=>setFolders(values=>values.map((value,i)=>i===index?{mode,path:''}:value))} options={[{value:'new',label:'Create a new folder'},{value:'existing',label:'Use an existing folder'}]}/></Field>{folder.mode==='new'?<p className="k-field__hint">Creates a private folder under <span className="project-path">{state.root}</span>.</p>:<><Field label={`Existing folder path ${index+1}`} ><Input required placeholder="/home/you/work/project" value={folder.path} disabled={state.locked} onChange={event=>setFolders(values=>values.map((value,i)=>i===index?{...value,path:event.target.value}:value))}/></Field><Button size="sm" disabled={state.locked} onClick={()=>setBrowser(index)}>Browse folders</Button></>}{<Button size="sm" disabled={state.locked} onClick={()=>setFolders(values=>values.filter((_,i)=>i!==index))}>Remove folder {index+1}</Button>}{browser===index&&<FolderBrowser request={request} onClose={()=>setBrowser(null)} onChoose={path=>{setFolders(values=>values.map((value,i)=>i===index?{mode:'existing',path}:value));setBrowser(null);}}/>}</div>)}<div><Button size="sm" disabled={state.locked || folders.length>=32} icon={<Plus size={14} aria-hidden/>} onClick={()=>setFolders(values=>[...values,{mode:values.length ? 'existing' : 'new',path:''}])}>Add folder</Button></div></Section><Section title="Members"><Members agents={agents} members={members} onChange={setMembers} disabled={state.locked}/></Section>{error&&<Alert tone="error">{error}</Alert>}</form></Dialog>;
}
function indexLabel(index: LocalProject['codegraphIndex']) {
  switch (index?.state) {
    case 'noFolders': return 'Add a folder to index code';
    case 'unavailable': return 'Unavailable on this host';
    case 'queued': return 'Indexing project folders';
    case 'error': return 'Indexing failed';
    case 'ready': {
      const count = index.files ?? 0;
      if (!count) return 'No supported source files';
      const files = `${count.toLocaleString()} ${count === 1 ? 'file' : 'files'} indexed`;
      return index.errors ? `${files} · ${index.errors.toLocaleString()} with errors` : files;
    }
    default: return 'Off';
  }
}
export function ProjectHome({project,agents,state,request,onChat,onThread,onReport,onDeleted}:{project:LocalProject;agents:Agent[];state:ProjectState;request:Request;onChat:(channel:string)=>void;onThread:(agent:string,thread:string)=>void;onReport:(agent:string,destination:string,turn:string)=>void;onDeleted:()=>void}) {
  const [tab,setTab]=useState('overview');
  const scopedRequest=useCallback<Request>((path,body)=>request(path==='/api/connected-apps/key'?path:path.replace(/^\/api\//,`/api/projects/${project.id}/`),body),[project.id,request]);
  const memberAgents=agents.filter(agent=>project.members.some(member=>member.agent===agent.id));
  const [members,setMembers]=useState(project.members),[editing,setEditing]=useState(false),[name,setName]=useState(project.name),[description,setDescription]=useState(project.description);
  const [folder,setFolder]=useState(project.folders[0] ?? ''),[folders,setFolders]=useState(project.folders),[folderPath,setFolderPath]=useState(''),[browser,setBrowser]=useState(false),[error,setError]=useState(''),[newSection,setNewSection]=useState(false);
  const confirm=useConfirm();
  useEffect(()=>{setMembers(project.members);setName(project.name);setDescription(project.description);setFolders(project.folders);setEditing(false);if(!project.folders.includes(folder))setFolder(project.folders[0] ?? '');},[project.revision,project.id]);
  async function save(){const write=members.some(member=>member.write&&(!project.members.some(old=>old.agent===member.agent&&old.write)||folders.some(path=>!project.folders.includes(path))));if(write&&!await confirm({title:'Grant project write access?',description:'Selected agents can create, change, and delete files in project folders. Access applies only in project sections and conversations.',confirm:'Grant write access'}))return;await state.execute(`/api/projects/${project.id}/edit`,{id:crypto.randomUUID(),revision:project.revision,name,description,members,folders,confirmWrite:write});}
  return <div className="k-page bs-root project-home"><PageHeader title={project.name} description={project.description || undefined} actions={<><ActionMenu label="Project menu" trigger={<IconButton label="Project menu" disabled={state.locked}><Ellipsis size={16} aria-hidden/></IconButton>} items={[{label:'Edit project',onSelect:()=>{setTab('overview');setEditing(true);}},{label:'Archive project',danger:true,onSelect:()=>archiveProject(project,state,confirm,onDeleted)}]}/></>}/>{state.recovery}{error&&<Alert tone="error">{error}</Alert>}
    <Tabs value={tab} onChange={setTab} label="Project sections" tabs={[{value:'overview',label:'Overview'},{value:'calendar',label:'Calendar'},{value:'apps',label:'Connected apps'},{value:'environment',label:'Environment'},{value:'mcp',label:'MCP'}]}>
    <TabPanel value="overview">
    {editing&&<Section title="Project details"><Field label="Project name"><Input disabled={state.locked} value={name} maxLength={80} onChange={event=>setName(event.target.value)}/></Field><Field label="Description" optional><Textarea disabled={state.locked} value={description} maxLength={2000} onChange={event=>setDescription(event.target.value)}/></Field></Section>}
    <Section title="Members" actions={!editing&&<Button size="sm" disabled={state.locked} onClick={()=>setEditing(true)}>Manage members & folders</Button>}>{editing?<Members agents={agents} members={members} onChange={setMembers} disabled={state.locked}/>:<Card>{project.members.map(member=>{const agent=agents.find(agent=>agent.id===member.agent);return <Row key={member.agent} title={agent?.name??'Agent'} leading={agent&&<AgentAvatar agent={agent} size={30}/>} trailing={<Badge>{member.write?'Can edit':'Read only'}</Badge>}/>;})}{!project.members.length&&<EmptyState title="No members yet" />}</Card>}</Section>
    <Section title="Folders" actions={!editing&&<Button size="sm" icon={<Plus size={14} aria-hidden/>} disabled={state.locked} onClick={()=>{setEditing(true);setBrowser(true);}}>Add folder</Button>}>{(editing?folders:project.folders).length ? <Card>{(editing?folders:project.folders).map(path=><Row key={path} leading={<Folder size={18} aria-hidden/>} title={<span className="project-path">{path}</span>} meta={path===(editing?folders:project.folders)[0]?'Default folder for sections':'Available to project sections and conversations'} trailing={editing&&<IconButton label={`Remove folder ${path}`} disabled={state.locked} onClick={()=>setFolders(values=>values.filter(value=>value!==path))}><X size={14} aria-hidden/></IconButton>}/>)}</Card>:<p className="k-field__hint">No folders</p>}
      {editing&&<><Field label="Add existing folder" ><Input value={folderPath} disabled={state.locked} onChange={event=>setFolderPath(event.target.value)}/></Field><div className="k-row-actions"><Button size="sm" disabled={state.locked||!folderPath||folders.length>=32} onClick={()=>{if(!folders.includes(folderPath))setFolders(values=>[...values,folderPath]);setFolderPath('');}}>Add folder</Button><Button size="sm" disabled={state.locked} onClick={()=>setBrowser(true)}>Browse folders</Button></div>{browser&&<FolderBrowser request={request} onClose={()=>setBrowser(false)} onChoose={path=>{setFolderPath(path);setBrowser(false);}}/>}<div className="k-row-actions"><Button variant="primary" disabled={state.locked || !name.trim()} busy={state.busy} onClick={()=>void save()}>Save project</Button><Button disabled={state.busy} onClick={()=>{setEditing(false);setMembers(project.members);setFolders(project.folders);}}>Cancel</Button></div></>}
    </Section>
    <Card><Row title="Minarbot CodeGraph"
      meta={<span className="project-index-status" role="status">{indexLabel(project.codegraphIndex)}</span>} trailing={
      <Switch label="Minarbot CodeGraph" checked={project.codegraphEnabled ?? false}
        disabled={state.locked || editing || !state.codegraphAvailable}
        onChange={enabled => void state.execute(`/api/projects/${project.id}/edit`, {
          id: crypto.randomUUID(), revision: project.revision, codegraphEnabled: enabled,
        })} />
    } /></Card>
    <Section title="Sections" actions={<Button size="sm" icon={<Plus size={14} aria-hidden/>} disabled={state.locked} onClick={()=>setNewSection(true)}>New section</Button>}><Card>{project.sections.map(section=><Row key={section.id} leading={<Hash size={16} aria-hidden/>} title={section.name} meta={section.archived?'Archived':undefined} onClick={()=>onChat(section.id)}/>)}</Card></Section>
    <Section title="Conversations" actions={<ActionMenu label="New conversation" trigger={<Button size="sm" icon={<Plus size={14} aria-hidden/>}>New conversation</Button>} items={project.members.length ? project.members.map(member=>({label:agents.find(agent=>agent.id===member.agent)?.name??'Agent',disabled:state.locked,onSelect:()=>{void (async()=>{const id=crypto.randomUUID();const ok=await state.execute(`/api/projects/${project.id}/thread`,{id,revision:project.revision,agent:member.agent,folder,name:'Project conversation'});if(ok){try{const fresh=await request<LocalProject>(`/api/projects/${project.id}`);const thread=fresh.threads.find(thread=>!project.threads.some(old=>old.id===thread.id));if(thread)onThread(thread.agent,thread.id);}catch(error){setError(message(error));}}})();}})) : [{label:'Add member',onSelect:()=>setEditing(true)}]}/>}>{project.folders.length>0&&<Select label="Conversation folder" value={folder} onChange={setFolder} options={project.folders.map(path=>({value:path,label:path}))}/>}{project.threads.length?<Card>{project.threads.map(thread=><Row key={thread.id} title={thread.name} meta={agents.find(agent=>agent.id===thread.agent)?.name} onClick={()=>onThread(thread.agent,thread.id)}/>)}</Card>:<p className="k-field__hint">No conversations</p>}</Section>
    {folder&&<ProjectSourceControl key={folder} project={project.id} folder={folder} request={request}/>}
    </TabPanel>
    <TabPanel value="calendar"><AutomationCalendar key={project.id} project={project} projects={state.projects} agents={agents} request={request} onThread={onThread} onReport={onReport} /></TabPanel>
    <TabPanel value="apps"><ConnectedAppsPanel key={`apps:${project.id}`} project={project} agents={memberAgents} request={scopedRequest} onInstallationAppsChange={async enabled=>{await state.execute(`/api/projects/${project.id}/edit`,{id:crypto.randomUUID(),revision:project.revision,allowInstallationApps:enabled,confirmed:enabled});}}/></TabPanel>
    <TabPanel value="environment"><ProjectEnvironmentPanel key={`env:${project.id}`} project={project.id} request={request}/></TabPanel>
    <TabPanel value="mcp"><McpPanel key={`mcp:${project.id}`} project={project.id} agents={memberAgents} request={scopedRequest} onClose={()=>setTab('overview')}/></TabPanel>
    </Tabs>
    {newSection&&<ProjectNameDialog title="New section" project={project} state={state} onClose={()=>setNewSection(false)}/>}
  </div>;
}
async function archiveProject(project:LocalProject,state:ProjectState,confirm:ReturnType<typeof useConfirm>,onArchived:()=>void){
  if(await confirm({title:`Archive project ${project.name}?`,description:'Sections and conversations become read-only. History, members, folder settings, and files stay saved.',confirm:'Archive project',danger:true})&&await state.execute(`/api/projects/${project.id}/archive`,{id:crypto.randomUUID(),revision:project.revision,confirmed:true}))onArchived();
}
function ProjectNameDialog({title,project,section,state,onClose}:{title:'New section'|'Rename section'|'Rename project';project:LocalProject;section?:LocalProject['sections'][number];state:ProjectState;onClose:()=>void}){
  const [name,setName]=useState(title==='Rename project'?project.name:section?.name??'');
  const form='project-name-form';
  return <Dialog open title={title}  onOpenChange={open=>{if(!open&&!state.busy)onClose();}} footer={<><Button disabled={state.busy} onClick={onClose}>Cancel</Button><Button variant="primary" form={form} type="submit" busy={state.busy} disabled={state.locked||!name.trim()}>{title==='New section'?'Create section':'Save name'}</Button></>}><form id={form} className="k-stack" onSubmit={async event=>{event.preventDefault();const path=title==='Rename project'?`/api/projects/${project.id}/edit`:`/api/projects/${project.id}/sections${section?`/${section.id}/edit`:''}`;if(await state.execute(path,{id:crypto.randomUUID(),revision:project.revision,name}))onClose();}}><Field label={title==='Rename project'?'Project name':'Section name'}><Input required maxLength={80} value={name} disabled={state.locked} onChange={event=>setName(event.target.value)}/></Field>{state.recovery}</form></Dialog>;
}
export function ProjectSidebar({state,selectedProject,selectedSection,search,onProject,onSection,onNew,onArchived}:{state:ProjectState;selectedProject:string;selectedSection:string;search:string;onProject:(id:string)=>void;onSection:(id:string)=>void;onNew:()=>void;onArchived:(id:string)=>void}){
  const projects = state.projects.filter(project=>`${project.name} ${project.description}`.toLocaleLowerCase().includes(search)||project.sections.some(section=>!section.archived&&section.name.toLocaleLowerCase().includes(search)));
  return <nav className="project-navigation" aria-label="Projects">
    {!state.projects.length ? <EmptyState icon={<Folder size={18} aria-hidden/>} title="No projects yet" action={<Button size="sm" onClick={onNew} icon={<Plus size={14} aria-hidden/>}>New project</Button>}></EmptyState> : <>
      {projects.map(project=><ProjectNavigation key={project.id} project={project} state={state} active={selectedProject===project.id} selectedSection={selectedSection} search={search} onProject={onProject} onSection={onSection} onArchived={onArchived}/>)}
      {!projects.length && <p className="machine-group__note">No matching projects or sections.</p>}
      <button type="button" className="project-navigation__new" onClick={onNew}><Plus size={14} aria-hidden/><span>New project</span></button>
    </>}{state.recovery}
  </nav>;
}
function ProjectNavigation({project,state,active,selectedSection,search,onProject,onSection,onArchived}:{project:LocalProject;state:ProjectState;active:boolean;selectedSection:string;search:string;onProject:(id:string)=>void;onSection:(id:string)=>void;onArchived:(id:string)=>void}){
  const key=`minarbot:sidebar-project:${project.id}`;
  const [expanded,setExpanded]=useState(()=>{try{return localStorage.getItem(key)!=='closed';}catch{return true;}}),[editing,setEditing]=useState<{title:'New section'|'Rename section'|'Rename project';section?:LocalProject['sections'][number]}|null>(null);
  const confirm=useConfirm(), visible=project.sections.filter(section=>!section.archived);
  const changeSection=(section:LocalProject['sections'][number],fields:Record<string,unknown>)=>state.execute(`/api/projects/${project.id}/sections/${section.id}/edit`,{id:crypto.randomUUID(),revision:project.revision,...fields});
  const projectMatch=`${project.name} ${project.description}`.toLocaleLowerCase().includes(search);
  const open=!!search||expanded;
  const setOpen=(value:boolean)=>{setExpanded(value);try{localStorage.setItem(key,value?'open':'closed');}catch{/* Keep usable without storage. */}};
  return <Collapsible.Root className="project-navigation__project" open={open} onOpenChange={setOpen}>
    <div className={`project-navigation__header${active?' is-selected':''}`}><Collapsible.Trigger asChild><IconButton label={`${open ? 'Collapse' : 'Expand'} project ${project.name}`}><ChevronRight size={14} aria-hidden className={open ? 'is-open' : undefined}/></IconButton></Collapsible.Trigger><button type="button" onClick={()=>onProject(project.id)} className="project-navigation__name" aria-current={active?'page':undefined} title={project.name} onKeyDown={event=>{if(event.key==='ArrowRight'||event.key==='ArrowLeft'){event.preventDefault();setOpen(event.key==='ArrowRight');}}}>{open?<FolderOpen size={16} aria-hidden/>:<Folder size={16} aria-hidden/>}<span>{project.name}</span></button><ActionMenu label={`Menu project ${project.name}`} trigger={<IconButton label={`Menu project ${project.name}`} tooltip="More actions" disabled={state.locked}><Ellipsis size={15} aria-hidden/></IconButton>} items={[{label:'New section',onSelect:()=>setEditing({title:'New section'})},{label:'Rename',onSelect:()=>setEditing({title:'Rename project'})},{label:'Project settings',onSelect:()=>onProject(project.id)},{label:'Archive',danger:true,onSelect:()=>archiveProject(project,state,confirm,()=>onArchived(project.id))}]}/></div>
    <Collapsible.Content><div className="project-navigation__sections">{visible.filter(section=>projectMatch||section.name.toLocaleLowerCase().includes(search)).map(section=>{const index=visible.indexOf(section),unread=section.activity>section.read_position,current=section.id===selectedSection;return <div className="project-navigation__section" key={section.id}><button type="button" className={`project-navigation__room${current?' is-selected':''}`} aria-label={`# ${section.name} in ${project.name}${unread?' - Unread':''}`} aria-current={current?'page':undefined} onClick={()=>onSection(section.id)}><Hash size={16} aria-hidden/><span>{section.name}</span>{unread&&<span className="project-navigation__unread" role="img" aria-label="Unread"/>}</button><ActionMenu label={`Menu section ${section.name} in ${project.name}`} trigger={<IconButton label={`Menu section ${section.name} in ${project.name}`} tooltip="More actions" disabled={state.locked}><Ellipsis size={14} aria-hidden/></IconButton>} items={[{label:'Rename section',onSelect:()=>setEditing({title:'Rename section',section})},{label:'Move section up',disabled:index===0,onSelect:()=>void changeSection(section,{position:visible[index-1].position})},{label:'Move section down',disabled:index===visible.length-1,onSelect:()=>void changeSection(section,{position:visible[index+1].position})},{label:'Archive section',danger:true,onSelect:async()=>{if(await confirm({title:`Archive section ${section.name}?`,description:'History, memory, and files stay saved. View archived sections from project home.',confirm:'Archive section',danger:true}))void changeSection(section,{archived:true});}}]}/></div>;})}{!visible.length&&<p className="machine-group__note">No active sections.</p>}</div></Collapsible.Content>
    {editing&&<ProjectNameDialog {...editing} project={project} state={state} onClose={()=>setEditing(null)}/>}
  </Collapsible.Root>;
}
export function ProjectSourceControl({project,folder,request,onOpenDiff}:{project:string;folder:string;request:Request;onOpenDiff?:()=>void}) {
  const gitRequest=useCallback<Request>((path,body)=>{const url=new URL(path,'http://local');const operation=url.pathname.split('/git')[1]??'';url.searchParams.set('folder',folder);return request(`/api/projects/${project}/git${operation}?${url.searchParams}`,body);},[project,folder,request]);
  return <SourceControl showEmpty={!onOpenDiff} agent={project} request={gitRequest} onOpenDiff={onOpenDiff}/>;
}

export { HelpPage } from './HelpPage';

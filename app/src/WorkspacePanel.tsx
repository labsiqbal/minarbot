import { useCachedData, useQueryVersion } from './hooks/useCachedQuery';
import { queryCache } from './hooks/queryCache';
import { downloadBlob } from './hooks/downloadBlob';
import { readJson } from './http';
import { useVisiblePolling } from './hooks/useVisiblePolling';
import { useCallback, useEffect, useRef, useState } from 'react';

import { ChevronLeft, ChevronRight, Download, FileText, Folder } from 'lucide-react';
import { Alert, Button, Card, CodeBlock, EmptyState, IconButton, PageHeader, Row, Section, SkeletonRows } from './ui/kit';

type Location = { path: string; file: boolean; after?: string; offset?: number; digest?: string };
type Page = { path: string; entries?: { name: string; kind: string }[]; next_after?: string | null; text?: string; digest?: string; next_offset?: number | null };

export function WorkspacePanel({ agent, request, onClose, basePath = `/api/agents/${agent.id}/workspace`, compact = false, onOpenFile }: { agent: { id: string; name: string }; request: <T>(path: string) => Promise<T>; onClose: () => void; basePath?: string; compact?: boolean; onOpenFile?: (path: string) => void }) {
  const [locations,setLocations] = useState<Location[]>([{path:'',file:false}]);
  const [error,setError] = useState(''), [reload,setReload] = useState(0);
  const [downloading,setDownloading] = useState(false), [downloadError,setDownloadError] = useState('');
  const heading = useRef<HTMLHeadingElement>(null), generation = useRef(0);
  const location = locations[locations.length-1];
  useEffect(() => {
    generation.current++; setError(''); setDownloading(false); setDownloadError(''); heading.current?.focus();
    return () => { generation.current++; };
  }, [basePath, location]);
  const params = new URLSearchParams({path:location.path});
  if(location.after !== undefined)params.set('after',location.after);
  if(location.offset !== undefined)params.set('offset',String(location.offset));
  if(location.digest !== undefined)params.set('digest',location.digest);
  const path = `${basePath}/${location.file?'file':'entries'}?${params}`;
  const { data: page } = useCachedData<Page>(path);
  const version = useQueryVersion();
  const root = useVisiblePolling(useCallback(async current => {
    try { await queryCache.refresh(path, () => request<Page>(path)); if (current()) setError(''); }
    catch(reason) { if(current())setError(reason instanceof Error ? reason.message : 'Workspace could not load. Try again.'); }
  }, [path, reload, request, version]));
  function open(next:Location){if(next.file && onOpenFile){onOpenFile(next.path);return;}generation.current++;setError('');setLocations(value=>[...value,next]);}
  function back(){generation.current++;setError('');setLocations(value=>value.slice(0,-1));}
  async function download() {
    if(downloading)return;
    const current=generation.current;setDownloading(true);setDownloadError('');
    try {
      const response=await fetch(`${basePath}/download?${new URLSearchParams({path:location.path})}`);
      if (!response.ok) await readJson(response, 'Download failed. Try again.');
      const blob=await response.blob();if(current!==generation.current)return;
      downloadBlob(blob, location.path.split('/').pop()!);
    } catch(reason) {if(current===generation.current)setDownloadError(reason instanceof TypeError?'Download interrupted. Check the connection and try again.':reason instanceof Error?reason.message:'Download failed. Try again.');}
    finally {if(current===generation.current)setDownloading(false);}
  }
  return <div ref={root} className={`${compact ? "k-stack" : "k-page"} bs-root`}>
    {!compact && <PageHeader headingRef={heading} title={`Workspace files for ${agent.name}`}
      description="Local files linked by the host owner. Previews are read-only; opening files here does not send their contents to the model."
      actions={<Button variant="ghost" onClick={onClose}>Back</Button>} />}
    <nav className="k-row-actions" aria-label="Workspace path">
      {!compact && <Button disabled={locations.length<2} onClick={back} icon={<ChevronLeft size={14} aria-hidden />}>Previous</Button>}
      <Button variant="ghost" onClick={()=>{generation.current++;setError('');setLocations([{path:'',file:false}]);}} icon={<Folder size={14} aria-hidden />}>{compact ? 'Workspace' : 'Root directory'}</Button>
      {compact && location.path.split('/').filter(Boolean).map((part, index, parts) => <Button key={index} variant="ghost" onClick={() => setLocations([{path: parts.slice(0, index + 1).join('/'), file: false}])}>{part}</Button>)}
    </nav>
    <Section title={compact ? undefined : <span style={{ overflowWrap: 'anywhere' }}>{location.path || 'Root directory'}</span>}>
      {location.file && <><div className="k-row-actions"><Button aria-disabled={downloading} aria-busy={downloading} onClick={()=>void download()} icon={<Download size={14} aria-hidden />}>{downloading?'Preparing download...':'Download original file'}</Button></div><p className="k-field__hint" style={{ margin: 0 }}>Text or binary, up to 10 MiB. Downloads use the current file version.</p>{downloadError&&<Alert tone="error">{downloadError}</Alert>}</>}
      {error && <Alert tone="error" actions={<Button onClick={()=>{generation.current++;setError('');setLocations(value=>[...value.slice(0,-1),{path:location.path,file:location.file}]);setReload(value=>value+1);}}>Retry</Button>}>{error}</Alert>}{!page ? !error && <><span className="sr-only" role="status">Loading workspace...</span><SkeletonRows /></> : location.file ? <>
        <p className="k-field__hint" style={{ margin: 0 }}>Starting at character {(location.offset??0)+1}. Maximum 8000 characters per page.</p>
        <CodeBlock label={`Contents of ${location.path}`}>{page.text || '(Empty file)'}</CodeBlock>
        {page.next_offset != null && <div className="k-pager"><IconButton label="Next text page" disabled={page.next_offset == null} onClick={()=>open({...location,offset:page.next_offset!,digest:page.digest})}><ChevronRight size={16} aria-hidden /></IconButton></div>}
      </> : <>
        {page.entries?.some(entry => entry.name !== '.git') ? <Card>{page.entries.filter(entry => entry.name !== '.git').map(entry=><Row key={entry.name} title={entry.name}
          leading={entry.kind==='directory'?<Folder size={16} aria-hidden />:<FileText size={16} aria-hidden />}
          meta={compact ? undefined : entry.kind==='directory'?'Directory':entry.kind==='file'?'File':'Unsupported'}
          disabled={!['file','directory'].includes(entry.kind)||/[\x00-\x1f\x7f\\]/.test(entry.name)}
          onClick={()=>open({path:[location.path,entry.name].filter(Boolean).join('/'),file:entry.kind==='file'})} />)}</Card> : <EmptyState icon={<Folder size={18} />} title="Directory is empty on this page." />}
        {page.next_after && <div className="k-pager"><IconButton label="Next entries" disabled={!page.next_after} onClick={()=>open({...location,after:page.next_after!})}><ChevronRight size={16} aria-hidden /></IconButton></div>}
      </>}
    </Section>
  </div>;
}

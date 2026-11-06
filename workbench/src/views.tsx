import { ChangeLinksView } from './change-links';
import { MigrationView } from './migration';
import { useState, type ReactNode } from 'react';
import { File, Folder, GitCommitHorizontal, ArrowUpRight } from 'lucide-react';
import { Button, Field, Input } from '@golia/ui/controls';
import { api, query, repoPath, type InstanceStatus, type InstanceAuditEntry, type Blob, type Commit, type Page, type RecordDetail, type RecordSummary, type Repository, type RequestSummary, type Review, type Session, type Tree } from './api';
import type { Document } from './documents';
import type { Text } from './i18n';
import { errorText, Failure, Loading, useResource } from './resource';

type Props = { document: Document; session: Session; t: Text; open: (document: Document) => void; log: (message: string) => void };
function Heading({ title, refresh, t, children }: { title: string; refresh: () => void; t: Text; children?: ReactNode }) {
  return <header className="document-heading"><h1>{title}</h1><div>{children}<Button small onClick={refresh}>{t.refresh}</Button></div></header>;
}
function Paged<T>({ path, t, title, row, empty, onRefresh }: { path: string; t: Text; title: string; row: (value: T) => ReactNode; empty?: string; onRefresh?: () => void }) {
  const [cursors, setCursors] = useState<string[]>(['']), [revision, refresh] = useState(0);
  const cursor = cursors.at(-1) ?? '';
  const resource = useResource<Page<T>>(`${path}${path.includes('?')?'&':'?'}after=${encodeURIComponent(cursor)}`, revision);
  const reload=()=>{onRefresh?.();refresh(value=>value+1);};
  return <><Heading title={title} t={t} refresh={reload}/>{resource.error ? <Failure error={resource.error} retry={reload} t={t}/>:!resource.value ? <Loading t={t}/>:<>
    <div className="resource-list">{resource.value.items.length ? resource.value.items.map(row) : <p className="message">{empty ?? t.empty}</p>}</div>
    <div className="pagination"><Button disabled={cursors.length===1} onClick={()=>setCursors(current=>current.slice(0,-1))}>{t.back}</Button><Button disabled={!resource.value.next} onClick={()=>setCursors(current=>[...current,resource.value!.next!])}>{t.next}</Button></div>
  </>}</>;
}
export function DocumentView(props: Props) {
  const { document, t, open } = props;
  switch (document.kind) {
    case 'repositories': return <Paged<Repository> path="/web/repositories" t={t} title={t.repositories} row={repository=><button key={repository.scope.repository_instance_id} className="resource-row" onClick={()=>open({kind:'repository',repository:repository.scope.repository_instance_id})}><Folder size={18}/><strong>{repository.scope.repository_instance_id}</strong><span>{repository.writes_enabled?t.writable:t.readOnly}</span><ArrowUpRight size={14}/></button>}/>;
    case 'inbox': return <Paged<RequestSummary> path="/web/inbox" t={t} title={t.inbox} empty={t.noReview} row={request=><button key={`${request.key.scope.repository_instance_id}:${request.key.actor_id}:${request.key.request_id}`} className="resource-row" onClick={()=>open({kind:'review',repository:request.key.scope.repository_instance_id,actor:request.key.actor_id,request:request.key.request_id,epoch:request.key.scope.recovery_epoch})}><strong>{request.key.scope.repository_instance_id}</strong><span>{request.key.actor_id} · {request.key.request_id}</span><span>{t.pending}</span></button>}/>;
    case 'repository': return <RepositoryView {...props} repository={document.repository}/>;
    case 'tasks': case 'deliveries': return <Paged<RecordSummary> path={repoPath(document.repository,`/records/${document.kind==='tasks'?'task':'delivery'}`)} t={t} title={t[document.kind]} row={record=><button key={record.id} className="resource-row" onClick={()=>open({kind:'record',repository:document.repository,recordKind:record.kind,id:record.id})}><strong>{record.title}</strong><span>{record.published_by}</span><time>{new Date(Number(record.published_at)).toLocaleString()}</time><code>{record.id.slice(0,12)}</code></button>}/>;
    case 'history': return <Paged<Commit> path={repoPath(document.repository,'/commits')} t={t} title={t.firstParentHistory} row={commit=><button key={commit.oid} className="resource-row" onClick={()=>open({kind:'tree',repository:document.repository,commit:commit.oid,path:''})}><GitCommitHorizontal size={18}/><strong>{commit.message.split('\n')[0]}</strong><span>{commit.author}</span><time>{new Date(Number(commit.timestamp)*1000).toLocaleString()}</time><code>{commit.oid.slice(0,12)}</code></button>}/>;
    case 'tree': return <TreeView {...props} document={document}/>;
    case 'blob': return <BlobView {...props} document={document}/>;
    case 'record': return <RecordView {...props} document={document}/>;
    case 'review': return <ReviewView {...props} document={document}/>;
    case 'migration': return <MigrationView repository={document.repository} session={props.session} t={t} log={props.log}/>;
    case 'associations':case 'deliveryLinks':return <ChangeLinksView repository={document.repository} delivery={document.kind==='deliveryLinks'?document.delivery:undefined} session={props.session} t={t} open={open}/>;
    case 'settings': return <SettingsView {...props}/>;
  }
}
function RepositoryView({ repository, t, open, session }: Props & { repository: string }) {
  const [revision, refresh] = useState(0);
  const resource = useResource<Repository & { refs: Record<string,string> }>(repoPath(repository),revision);
  return <><Heading title={repository} t={t} refresh={()=>refresh(value=>value+1)}/>{resource.error?<Failure error={resource.error} t={t} retry={()=>refresh(value=>value+1)}/>:!resource.value?<Loading t={t}/>:<>
    <div className="document-actions"><Button onClick={()=>open({kind:'history',repository})}>{t.history}</Button><Button onClick={()=>open({kind:'tasks',repository})}>{t.tasks}</Button><Button onClick={()=>open({kind:'deliveries',repository})}>{t.deliveries}</Button><Button onClick={()=>open({kind:'associations',repository})}>{t.associations}</Button>{session.user.administrator&&<Button onClick={()=>open({kind:'migration',repository})}>{t.migration}</Button>}</div>
    <h2>{t.branch}</h2><div className="resource-list">{Object.entries(resource.value.refs).map(([name,oid])=><button key={name} className="resource-row" onClick={()=>open({kind:'tree',repository,commit:oid,path:''})}><strong>{name}</strong><code>{oid}</code>{name===resource.value!.default_branch&&<span>HEAD</span>}</button>)}</div>
    {Object.keys(resource.value.refs).length===0&&<p className="message">{t.empty}</p>}
  </>}</>;
}
function TreeView({ document, t, open }: Props & { document: Extract<Document,{kind:'tree'}> }) {
  const [revision,refresh]=useState(0);
  const resource=useResource<Tree>(repoPath(document.repository,'/tree')+query({commit:document.commit,path:document.path}),revision);
  return <><Heading title={resource.value?.display_path||t.files} t={t} refresh={()=>refresh(value=>value+1)}/>{resource.value?.parent!==null&&resource.value?.parent!==undefined&&<Button small onClick={()=>open({...document,path:resource.value!.parent!})}>{t.back}</Button>}
    {resource.error?<Failure error={resource.error} t={t} retry={()=>refresh(value=>value+1)}/>:!resource.value?<Loading t={t}/>:<><code className="commit-id">{resource.value.commit}</code><div className="resource-list">{resource.value.entries.map(entry=><button key={entry.path} className="resource-row" disabled={entry.kind==='gitlink'} onClick={()=>entry.kind==='tree'?open({...document,commit:resource.value!.commit,path:entry.path}):open({kind:'blob',repository:document.repository,oid:entry.oid,path:entry.display_path})}>{entry.kind==='tree'?<Folder size={16}/>:<File size={16}/>}<strong>{entry.name}</strong><span>{entry.kind}</span><span>{entry.bytes??''}</span></button>)}</div></>}
  </>;
}
function safeDownload(url: string) { return url.startsWith('/web/') && !url.startsWith('//') ? url : undefined; }
function BlobView({ document,t }: Props & {document:Extract<Document,{kind:'blob'}>}) {
  const [revision,refresh]=useState(0);
  const resource=useResource<Blob>(repoPath(document.repository,`/objects/${document.oid}`),revision);
  return <><Heading title={document.path} t={t} refresh={()=>refresh(value=>value+1)}/>{resource.error?<Failure error={resource.error} t={t} retry={()=>refresh(value=>value+1)}/>:!resource.value?<Loading t={t}/>:<>
    <div className="document-actions"><code>{resource.value.oid}</code><span>{BigInt(resource.value.bytes).toLocaleString()} B</span><a href={safeDownload(resource.value.download_url)} download>{t.download}</a></div>
    {resource.value.truncated&&<p className="message">{t.truncated}</p>}{resource.value.text===null?<p className="message">{t.binary}</p>:<pre className="source"><code>{resource.value.text}</code></pre>}
  </>}</>;
}
function RecordView({document,t,open}:Props & {document:Extract<Document,{kind:'record'}>}) {
  const [revision,refresh]=useState(0);
  const resource=useResource<RecordDetail>(repoPath(document.repository,`/records/${document.recordKind}/${document.id}`),revision);
  return <><Heading title={t[document.recordKind]} t={t} refresh={()=>refresh(value=>value+1)}/>{resource.error?<Failure error={resource.error} t={t} retry={()=>refresh(value=>value+1)}/>:!resource.value?<Loading t={t}/>:<>
    {document.recordKind==='delivery'&&<Button onClick={()=>open({kind:'deliveryLinks',repository:document.repository,delivery:document.id})}>{t.deliveryLinks}</Button>}
    <h2>{resource.value.summary.title}</h2><p><code>{resource.value.summary.id}</code></p><dl className="record-fields">{resource.value.fields.map(field=><div key={field.label}><dt>{field.label in t?t[field.label as keyof Text]:field.label}</dt><dd>{field.value}</dd></div>)}</dl>
    {resource.value.fields_truncated&&<p>{t.truncated}</p>}<h2>{t.source}</h2><p>{t.publishedBy}: {resource.value.publication.actor_id}</p><p><code>{resource.value.publication.digest}</code> · {resource.value.publication.sequence}</p>
    <h2>{t.related}</h2><div className="resource-list">{resource.value.related.map(record=><button className="resource-row" key={`${record.kind}:${record.id}`} onClick={()=>open({kind:'record',repository:document.repository,recordKind:record.kind,id:record.id})}><strong>{t[record.kind]}</strong><span>{record.title}</span><code>{record.id.slice(0,12)}</code></button>)}</div>
    <h2>{t.attachments}</h2><ul>{resource.value.attachments.map(attachment=><li key={attachment.name}>{attachment.name} · {BigInt(attachment.bytes).toLocaleString()} B {attachment.download_url?<a href={safeDownload(attachment.download_url)} download>{t.download}</a>:<span>{t.omitted}: {attachment.omitted_reason}</span>}</li>)}</ul>
  </>}</>;
}
function ReviewView({document,t,log}:Props & {document:Extract<Document,{kind:'review'}>}) {
  const [revision,refresh]=useState(0),[busy,setBusy]=useState(false),[result,setResult]=useState<string>(),[failure,setFailure]=useState<unknown>();
  const path=repoPath(document.repository,`/epochs/${document.epoch}/requests/${encodeURIComponent(document.actor)}/${encodeURIComponent(document.request)}`);
  const resource=useResource<Review>(`${path}/review`,revision);
  return <><Heading title={t.review} t={t} refresh={()=>{refresh(value=>value+1);setResult(undefined);setFailure(undefined);}}/>{resource.error?<Failure error={resource.error} t={t} retry={()=>refresh(value=>value+1)}/>:!resource.value?<Loading t={t}/>:<>
    <dl className="record-fields"><div><dt>{t.candidate}</dt><dd><code>{resource.value.candidate_id}</code></dd></div><div><dt>{t.historicalReview}</dt><dd>{resource.value.historical_reviewer}</dd></div></dl>
    <h2>{t.changes}</h2><p><a href={safeDownload(resource.value.original_changes_url)} download>{t.download}</a></p>{resource.value.changes_truncated&&<p>{t.truncated}</p>}{resource.value.changes.map(change=><section className="change" key={change.path}><h3>{change.path}</h3><p>{change.kind} · {change.before?.slice(0,12)??'∅'} → {change.after?.slice(0,12)??'∅'}</p>{change.patch===null?<p>{t.truncated}</p>:<pre className="patch"><code>{change.patch.split('\n').map((line,index)=><span key={index} className={line.startsWith('+')?'added':line.startsWith('-')?'removed':''}>{line}{'\n'}</span>)}</code></pre>}</section>)}
    <h2>{t.evidence}</h2><div className="evidence-list">{resource.value.evidence.map(evidence=><article key={evidence.id}><strong>{evidence.command}</strong><p>{evidence.outcome} · {evidence.source}</p><p className={evidence.accepted?'success':'attention'}>{evidence.accepted?t.accepted:t.notAccepted}: {evidence.reason}</p></article>)}</div>
    <section className="approval"><h2>{t.currentDecision}</h2><p>{resource.value.approved?t.approved:resource.value.eligible?t.approveHint:resource.value.reason}</p>{failure !== undefined &&<p role="alert" className="failure">{errorText(failure,t)}</p>}{result&&<p role="status">{result}</p>}
    <Button variant="primary" disabled={resource.value.approved||!resource.value.eligible||!!result||!!failure} busy={busy} onClick={async()=>{
      const selection=resource.value!;setBusy(true);setFailure(undefined);
      try {await api.request(`${path}/approve`,{integration_id:selection.integration_id,inspected:selection.inspected});setResult(t.approved);log(`${t.approved} · ${document.repository} · ${document.request}`);}
      catch(error){setFailure(error);log(errorText(error,t));}finally{setBusy(false);}
    }}>{t.approve}</Button></section>
  </>}</>;
}
type LocalUser={actor_id:string;login_name:string;display_name:string;administrator:boolean;disabled:boolean};
function SettingsView({session,t,log}:Props) {
  const [revision,refresh]=useState(0),[busy,setBusy]=useState(false),[failure,setFailure]=useState<unknown>();
  return <><Heading title={t.settings} t={t} refresh={()=>refresh(value=>value+1)}/><dl className="record-fields"><div><dt>{t.instance}</dt><dd>{session.installation_id}</dd></div><div><dt>{t.workspace}</dt><dd>{session.workspace_id}</dd></div><div><dt>{t.version}</dt><dd>{session.build_id}</dd></div><div><dt>{t.account}</dt><dd>{session.user.display_name}</dd></div></dl>
    {session.user.administrator&&session.deployment_mode==='self_hosted'&&<InstanceOperations t={t} revision={revision}/>}
    <Sessions t={t} log={log}/><Sessions client t={t} log={log}/><AgentCredentials t={t} log={log}/>
    {session.user.administrator&&session.deployment_mode==='self_hosted'&&<><p>{t.adminNote}</p><Paged<LocalUser> key={revision} path="/web/users" title={t.users} t={t} row={user=><div className="resource-row" key={user.actor_id}><strong>{user.display_name}</strong><span>{user.login_name}</span><span>{user.disabled?t.disabled:t.enabled}</span><Button small disabled={busy||user.actor_id===session.user.actor_id} onClick={async()=>{
      setBusy(true);setFailure(undefined);try{await api.request(`/web/users/${encodeURIComponent(user.actor_id)}/state`,{disabled:!user.disabled});refresh(value=>value+1);log(t.saved);}catch(error){setFailure(error);}finally{setBusy(false);}
    }}>{user.disabled?t.enable:t.disable}</Button></div>}/><h2>{t.createUser}</h2><form className="user-form" onSubmit={async event=>{
      event.preventDefault();const form=event.currentTarget,fields=new FormData(form);setBusy(true);setFailure(undefined);
      try{await api.request('/web/users',{login_name:fields.get('login'),display_name:fields.get('display'),password:fields.get('password'),administrator:fields.get('administrator')==='on'});form.reset();refresh(value=>value+1);log(t.saved);}catch(error){setFailure(error);}finally{setBusy(false);}
    }}><Field label={t.loginName}><Input name="login" autoComplete="off" maxLength={128} required/></Field><Field label={t.displayName}><Input name="display" maxLength={256} required/></Field><Field label={t.password}><Input name="password" type="password" autoComplete="new-password" minLength={12} maxLength={256} required/></Field><label><input type="checkbox" name="administrator"/> {t.administrator}</label><Button type="submit" busy={busy}>{t.createUser}</Button></form></>}
    {failure !== undefined &&<p role="alert" className="failure">{errorText(failure,t)}</p>}
  </>;
}

type BrowserSession = { id:string; created_at_unix_ms:string; expires_at_unix_ms:string; current:boolean };
function Sessions({t,log,client=false}:{t:Text;log:(message:string)=>void;client?:boolean}) {
  const [revision,refresh]=useState(0),[busy,setBusy]=useState(false),[failure,setFailure]=useState<unknown>();
  const path=client?'/web/client-credentials':'/web/sessions';
  const resource=useResource<BrowserSession[]>(path,revision);
  const reload=()=>{setFailure(undefined);refresh(value=>value+1);};
  return <section><Heading title={client?t.clientCredentials:t.sessions} t={t} refresh={reload}/>{resource.error?<Failure error={resource.error} t={t} retry={reload}/>:!resource.value?<Loading t={t}/>:<div className="resource-list">{resource.value.map(session=><div className="resource-row" key={session.id}><strong>{client?t.cliCredential:session.current?t.currentSession:t.session}</strong><span>{t.createdAt}: {new Date(Number(session.created_at_unix_ms)).toLocaleString()}</span><span>{t.expiresAt}: {new Date(Number(session.expires_at_unix_ms)).toLocaleString()}</span><Button small disabled={busy||session.current||failure!==undefined} onClick={async()=>{setBusy(true);setFailure(undefined);try{await api.request(`${path}/${session.id}/revoke`,{});refresh(value=>value+1);log(t.saved);}catch(error){setFailure(error);}finally{setBusy(false);}}}>{client?t.revokeCredential:t.revokeSession}</Button></div>)}</div>}{failure!==undefined&&<p role="alert" className="failure">{errorText(failure,t)}</p>}</section>;
}

type AgentCredential={credential_id:string;purpose:string;attribution:{agent_session:string};scope:{repository:{repository_instance_id:string;recovery_epoch:string};publish_records:boolean;grants:{refs:{Exact?:string;Namespace?:string};create:boolean;update:boolean;delete:boolean;force:boolean;paths:number[][]|null}[]};expires_at_unix_ms:string;revoked:boolean};
function displayGrantPath(bytes:number[]):string {
  try { return JSON.stringify(new TextDecoder('utf-8',{fatal:true}).decode(new Uint8Array(bytes))); }
  catch { return bytes.map(byte=>`\\x${byte.toString(16).padStart(2,'0')}`).join(''); }
}
function AgentCredentials({t,log}:{t:Text;log:(message:string)=>void}) {
  const [revision,refresh]=useState(0),[busy,setBusy]=useState(false),[failure,setFailure]=useState<unknown>();
  return <section className="agent-credentials"><p>{t.agentScopeNote}</p><Paged<AgentCredential> key={revision} path="/web/delegations" title={t.agentCredentials} t={t} onRefresh={()=>setFailure(undefined)} row={agent=><article className="agent-credential" key={agent.credential_id}>
    <h3>{agent.purpose}</h3><p>{t.repository}: {agent.scope.repository.repository_instance_id} · {t.recoveryEpoch}: {agent.scope.repository.recovery_epoch}</p><p>{t.session}: <code>{agent.attribution.agent_session}</code></p><p>{t.expiresAt}: {new Date(Number(agent.expires_at_unix_ms)).toLocaleString()} · {agent.revoked?t.revoked:Number(agent.expires_at_unix_ms)<=Date.now()?t.expiredRequest:t.agentRecorded}</p>
    <p>{agent.scope.publish_records?t.agentRecords:t.agentNoRecords}</p><ul>{agent.scope.grants.map((grant,index)=><li key={index}><code>{grant.refs.Exact??grant.refs.Namespace}</code> · {[grant.create&&t.agentCreate,grant.update&&t.agentUpdate,grant.delete&&t.agentDelete,grant.force&&t.agentForce].filter(Boolean).join(' · ')||t.readOnly}<p>{t.allowedPaths}: {grant.paths===null?t.agentAllPaths:grant.paths.length===0?t.agentNoPaths:grant.paths.map(displayGrantPath).join(', ')}</p></li>)}</ul>
    <Button small disabled={busy||agent.revoked||failure!==undefined} onClick={async()=>{setBusy(true);setFailure(undefined);try{await api.request(`/web/delegations/${agent.credential_id}/revoke`,{});refresh(value=>value+1);log(t.saved);}catch(error){setFailure(error);}finally{setBusy(false);}}}>{t.revokeCredential}</Button>
  </article>}/>{failure!==undefined&&<p role="alert" className="failure">{errorText(failure,t)}</p>}</section>;
}

function InstanceOperations({t,revision}:{t:Text;revision:number}) {
  const [retry,setRetry]=useState(0);
  const resource=useResource<InstanceStatus>('/web/instance',revision+retry);
  return <section key={retry}><h2>{t.instanceHealth}</h2>{resource.error?<Failure error={resource.error} t={t} retry={()=>setRetry(value=>value+1)}/>:!resource.value?<Loading t={t}/>:<>
    <p>{t.healthReadOnly}</p><dl className="record-fields">
      {([[t.schema,resource.value.schema],[t.generation,resource.value.runtime_generation],[t.repositories,resource.value.repositories],[t.storedBytes,resource.value.stored_bytes],[t.reservedBytes,resource.value.reserved_bytes],[t.pendingRequests,resource.value.pending_requests],[t.recoveryGaps,resource.value.recovery_gaps],[t.pendingEvents,resource.value.pending_events]] as const).map(([label,value])=><div key={label}><dt>{label}</dt><dd>{value}</dd></div>)}
      <div><dt>{t.observedAt}</dt><dd><time title={new Date(Number(resource.value.observed_at_unix_ms)).toISOString()}>{new Date(Number(resource.value.observed_at_unix_ms)).toLocaleString()}</time></dd></div>
      <div><dt>{t.lastRestore}</dt><dd>{resource.value.last_restore?new Date(Number(resource.value.last_restore.occurred_at_unix_ms)).toLocaleString():t.noRestoreRecorded}</dd></div>
    </dl><h3>{t.backupRecovery}</h3><p>{t.backupInstructions}</p><p>{t.backupUnconfirmed}</p>
    <Paged<InstanceAuditEntry> key={`${revision}-${retry}`} path="/web/instance/audit" title={t.operationHistory} t={t} row={entry=><div className="resource-row" key={entry.sequence}><strong>{entry.action}</strong><span>{entry.actor_id}</span><code>{entry.subject_id}</code><time title={new Date(Number(entry.occurred_at_unix_ms)).toISOString()}>{new Date(Number(entry.occurred_at_unix_ms)).toLocaleString()}</time></div>}/>
  </>}</section>;
}

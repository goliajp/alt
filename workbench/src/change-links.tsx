import { useEffect, useRef, useState } from 'react';
import { Button, Field, Input, NativeSelect } from '@golia/ui/controls';
import { api, repoPath, type Repository, type Session } from './api';
import type { Document } from './documents';
import type { Text } from './i18n';
import { errorText, Failure, Loading, useResource } from './resource';
import type { Channel, LinkHistory, LinkPage, LinkSummary, Release } from './change-link-model';
import { remember, journals, ownerKey, journalId, type Cursor, type Journal } from './link-journal';

type Props={repository:string;delivery?:string;session:Session;t:Text;open:(v:Document)=>void};
export function ChangeLinksView(props:Props) {
  const [revision,refresh]=useState(0);
  const info=useResource<Repository>(repoPath(props.repository),revision);
  return <section className="change-links"><header className="document-heading"><h1>{props.delivery?props.t.deliveryLinks:props.t.associations}</h1><Button onClick={()=>refresh(v=>v+1)}>{props.t.refresh}</Button></header>
    {info.error?<Failure error={info.error} t={props.t} retry={()=>refresh(v=>v+1)}/>:!info.value?<Loading t={props.t}/>:<Links key={JSON.stringify([info.value.scope,props.session.user.actor_id])} {...props} scope={info.value.scope} revision={revision}/>}
  </section>;
}
function Links({repository,scope,delivery,session,t,open,revision}:{scope:Repository['scope'];revision:number}&Props) {
  const base=repoPath(repository,`/epochs/${scope.recovery_epoch}/changes`);
  const [updated,refresh]=useState(0);
  const channels=useResource<Channel[]>(`${base}/channels`,revision+updated);
  const [cursors,setCursors]=useState<string[]>(['']);
  const after=cursors.at(-1)??'';
  const events=useResource<LinkPage>(`${base}/events${after?`?after=${after}`:''}`,revision+updated);
  const [picked,setPicked]=useState<LinkSummary>();
  const [previous,setPrevious]=useState<{digest:string;release:Release}>();
  return <>
    <p>{t.linkHistoryHint}</p>{delivery&&<p>{t.delivery}: <code>{delivery}</code></p>}
    <AssociationRequests scope={scope} delivery={delivery} session={session} channels={channels.value??[]} base={base} previous={previous} t={t} refresh={()=>refresh(v=>v+1)}/>
    {channels.error?<Failure error={channels.error} t={t} retry={()=>refresh(v=>v+1)}/>:!channels.value?<Loading t={t}/>:<>
      {!channels.value.some(c=>c.enabled)&&<p>{t.linkNoChannels}</p>}
      <h2>{t.linkHistory}</h2>
      {events.error?<Failure error={events.error} t={t} retry={()=>refresh(v=>v+1)}/>:!events.value?<Loading t={t}/>:<>
        <div className="resource-list">{events.value.entries.map(event=><button className="resource-row" key={event.digest} onClick={()=>setPicked(event)}><strong>{kind(event.kind,t)}</strong><span>{event.channel}</span><span>{event.pending?t.linkPending:event.decision?.state==='applied'?t.linkReceived:event.decision?t.linkRefused:t.linkHistory}</span><code>{event.digest.slice(0,12)}</code></button>)}{events.value.entries.length===0&&<p>{t.empty}</p>}</div>
        <div className="pagination"><Button disabled={cursors.length===1} onClick={()=>setCursors(v=>v.slice(0,-1))}>{t.back}</Button><Button disabled={!events.value.next} onClick={()=>setCursors(v=>[...v,events.value!.next!])}>{t.next}</Button></div>
      </>}
      {picked&&<EventDetail key={picked.digest} summary={events.value?.entries.find(e=>e.digest===picked.digest)??picked} base={base} t={t} channels={channels.value} revision={revision+updated} open={open} repository={repository} refresh={()=>refresh(v=>v+1)} usePrevious={setPrevious}/>}
    </>}
  </>;
}
function AssociationRequests({scope,delivery,session,channels,base,previous,t,refresh}:{scope:Repository['scope'];delivery?:string;session:Session;channels:Channel[];base:string;previous?:{digest:string;release:Release};t:Text;refresh:()=>void}) {
  const owner=ownerKey(scope.installation_id,session.user.actor_id,scope.repository_instance_id);
  const [items,setItems]=useState<Journal[]>([]),[cursor,setCursor]=useState<Cursor|null>(null),[selected,setSelected]=useState<Journal>();
  const [result,setResult]=useState<LinkHistory|null>(),[checked,setChecked]=useState(false),[failure,setFailure]=useState<unknown>(),[busy,setBusy]=useState(false);
  const running=useRef(false);
  const focusAfter=useRef<HTMLElement|null>(null);
  useEffect(()=>{if(!busy&&focusAfter.current){const node=focusAfter.current;focusAfter.current=null;if(document.activeElement===document.body&&node.isConnected)node.focus();}},[busy]);
  useEffect(()=>{let active=true;journals(owner).then(page=>{if(active){setItems(page.items);setCursor(page.next);}},e=>{if(active)setFailure(e);});return()=>{active=false;};},[owner]);
  const run=async(action:()=>Promise<void>)=>{if(running.current)return;running.current=true;focusAfter.current=document.activeElement instanceof HTMLElement?document.activeElement:null;setBusy(true);setFailure(undefined);try{await action();}catch(e){setFailure(e);}finally{running.current=false;setBusy(false);}};
  const save=async(record:Journal)=>{await remember(record);setItems(v=>[record,...v]);setSelected(record);setResult(undefined);setChecked(true);};
  const key=()=>({scope,actor_id:session.user.actor_id,request_id:crypto.randomUUID()});
  const bind=()=>{
    if(!selected||selected.owner!==owner||JSON.stringify(selected.key.scope)!==JSON.stringify(scope)||selected.key.actor_id!==session.user.actor_id)throw new Error('change_binding');
    return selected;
  };
  const send=async()=>{
    const record=bind();setChecked(false);
    const available=await api.request<Channel[]>(`${base}/channels`);
    if(!available.some(c=>c.enabled&&c.channel===record.intent.channel&&c.configuration_digest===record.intent.configuration_digest))throw new Error('change_binding');
    const response=await api.request<LinkHistory>(`${base}/${record.kind==='mapping'?'mappings':'withdrawals'}`,record.intent);
    setResult(response);refresh();
  };
  const check=async()=>{const record=bind();setResult(await api.request<LinkHistory|null>(`${base}/requests/${encodeURIComponent(record.key.actor_id)}/${encodeURIComponent(record.key.request_id)}`));setChecked(true);refresh();};
  return <section className="link-requests">
    {delivery&&channels.some(c=>c.enabled)&&<details open={!selected}><summary>{t.linkPrepare}</summary><MappingForm key={previous?.digest??'new'} channels={channels.filter(c=>c.enabled)} previous={previous} disabled={busy||!!selected} t={t} prepare={(channel,release,last)=>run(async()=>{const request=key();await save({version:1,id:journalId(owner,request.request_id),owner,created:Date.now(),key:request,kind:'mapping',intent:{key:request,channel:channel.channel,configuration_digest:channel.configuration_digest,delivery_id:delivery,release,expected_previous:last}});})}/></details>}
    {previous&&<details><summary>{t.linkWithdraw}</summary><form className="user-form" onSubmit={event=>{event.preventDefault();const data=new FormData(event.currentTarget);void run(async()=>{const channel=channels.find(c=>c.enabled&&c.channel===data.get('channel'));if(!channel)throw new Error('change_binding');const request=key();await save({version:1,id:journalId(owner,request.request_id),owner,created:Date.now(),key:request,kind:'withdraw',intent:{key:request,channel:channel.channel,configuration_digest:channel.configuration_digest,mapping_event_digest:previous.digest,expected_previous:previous.digest,reason:String(data.get('reason'))}});});}}>
      <Field label={t.linkChannel}><NativeSelect name="channel" disabled={busy||!!selected}>{channels.filter(c=>c.enabled).map(c=><option key={c.channel}>{c.channel}</option>)}</NativeSelect></Field><p><code>{previous.digest}</code></p><Field label={t.linkReason}><Input name="reason" required maxLength={1024} disabled={busy||!!selected}/></Field><Button type="submit" disabled={busy||!!selected}>{t.linkWithdraw}</Button>
    </form></details>}
    {items.length>0&&<Field label={t.linkRequest}><NativeSelect disabled={busy} value={selected?.id??''} onChange={e=>{setSelected(items.find(i=>i.id===e.target.value));setResult(undefined);setChecked(false);setFailure(undefined);}}><option value="">{t.linkNew}</option>{items.map(item=><option key={item.id} value={item.id}>{new Date(item.created).toLocaleString()} · {item.key.request_id}</option>)}</NativeSelect></Field>}
    {cursor&&<Button disabled={busy} onClick={()=>void run(async()=>{const page=await journals(owner,cursor);setItems(v=>[...v,...page.items]);setCursor(page.next);})}>{t.linkOlder}</Button>}
    {selected&&<section className="link-intent"><h3>{selected.kind==='mapping'?t.linkPrepare:t.linkWithdraw}</h3><p><code>{selected.key.request_id}</code></p><p>{selected.intent.channel}</p>{'delivery_id' in selected.intent?<><p>{t.delivery}: <code>{selected.intent.delivery_id}</code></p><ReleaseFields value={selected.intent.release} t={t}/></>:<p>{selected.intent.reason} · <code>{selected.intent.mapping_event_digest}</code></p>}<p>{t.linkIntentHint}</p><div className="document-actions"><Button disabled={busy||!checked||!!result} onClick={()=>void run(send)}>{t.linkSend}</Button><Button disabled={busy} onClick={()=>void run(check)}>{t.linkQuery}</Button></div>{result===null&&<p role="status">{t.linkNotSeen}</p>}{result&&<p role="status">{result.acknowledgement?result.acknowledgement.receipt.body.decision.state==='applied'?t.linkReceived:t.linkRefused:t.linkPending}</p>}</section>}
    {failure!==undefined&&<p className="failure" role="alert">{failure instanceof Error&&failure.message.startsWith('change_journal')?t.linkJournalError:errorText(failure,t)}</p>}
  </section>;
}
function MappingForm({channels,previous,disabled,t,prepare}:{channels:Channel[];previous?:{digest:string;release:Release};disabled:boolean;t:Text;prepare:(channel:Channel,release:Release,previous:string|null)=>Promise<void>}) {
  return <form className="user-form link-form" onSubmit={event=>{event.preventDefault();const data=new FormData(event.currentTarget),channel=channels.find(c=>c.channel===data.get('channel'));if(channel)void prepare(channel,{application:channel.application,release:String(data.get('release')),release_name:String(data.get('release_name')),environment:String(data.get('environment')),artifact_sha256:String(data.get('artifact'))},String(data.get('previous'))||null);}}><p>{t.linkMappingHint}</p>
    <Field label={t.linkChannel}><NativeSelect name="channel" disabled={disabled}>{channels.map(c=><option value={c.channel} key={c.channel}>{c.channel} · {c.application}</option>)}</NativeSelect></Field>
    <Field label={t.linkReleaseId}><Input name="release" required pattern="[A-Za-z0-9_-]+" maxLength={128} disabled={disabled} defaultValue={previous?.release.release}/></Field>
    <Field label={t.linkReleaseName}><Input name="release_name" required maxLength={200} disabled={disabled} defaultValue={previous?.release.release_name}/></Field>
    <Field label={t.linkEnvironment}><Input name="environment" maxLength={200} disabled={disabled} defaultValue={previous?.release.environment}/></Field>
    <Field label={t.linkArtifact}><Input name="artifact" required pattern="[0-9a-f]{64}" maxLength={64} disabled={disabled} defaultValue={previous?.release.artifact_sha256}/></Field>
    <Field label={t.linkPrevious}><Input name="previous" pattern="[0-9a-f]{64}" maxLength={64} disabled={disabled} defaultValue={previous?.digest}/></Field><Button type="submit" disabled={disabled}>{t.linkPrepare}</Button>
  </form>;
}
function EventDetail({summary,base,t,channels,revision,open,repository,refresh,usePrevious}:{summary:LinkSummary;base:string;t:Text;channels:Channel[];revision:number;open:(v:Document)=>void;repository:string;refresh:()=>void;usePrevious:(v:{digest:string;release:Release})=>void}) {
  const resource=useResource<LinkHistory>(`${base}/events/${summary.digest}`,revision);
  const [busy,setBusy]=useState(false),[failure,setFailure]=useState<unknown>();
  const history=resource.value,body=history?.statement.event.body;
  const channel=channels.find(c=>c.channel===summary.channel);
  return <section className="link-detail"><h2>{kind(summary.kind,t)}</h2><p><code>{summary.digest}</code></p>{resource.error?<Failure error={resource.error} t={t} retry={refresh}/>:!history?<Loading t={t}/>:<>
    <p>{t.source}: {history.statement.event.issuer.product} · {history.statement.event.issuer.installation} · {history.statement.event.issuer.scope}</p><p>{t.linkHistoryHint}</p>
    {body?.kind==='mapping'&&<><ReleaseFields value={body.value.release} t={t}/><dl className="record-fields"><div><dt>{t.commit}</dt><dd><code>{body.value.delivery.git_commit}</code></dd></div><div><dt>{t.delivery}</dt><dd><code>{body.value.delivery.delivery}</code></dd></div></dl><div className="document-actions"><Button onClick={()=>open({kind:'record',repository,recordKind:'delivery',id:body.value.delivery.delivery})}>{t.delivery}</Button>{channel?.enabled&&<Button onClick={()=>usePrevious({digest:summary.digest,release:body.value.release})}>{t.linkUsePrevious}</Button>}</div></>}
    {body?.kind==='withdraw'&&<p>{body.value.reason} · <code>{body.value.previous_mapping_digest}</code></p>}
    {body?.kind==='repair_request'&&<><p>{body.value.issue} · {body.value.target_ref}</p><ul>{body.value.selected_samples.map(s=><li key={s}><code>{s}</code></li>)}</ul><p>{t.linkRepairHint}</p>{channel?.peer_browser_origin&&<><a href={issueUrl(channel.peer_browser_origin,body.value.issue,body.value.selected_samples[0])} target="_blank" rel="noopener noreferrer">{t.linkOpenIssue}</a><p>{t.linkIndependent}</p></>}</>}
    {body?.kind==='task_bound'&&<><p>{t.linkRepairHint}</p><Button onClick={()=>open({kind:'record',repository,recordKind:'task',id:body.value.task})}>{t.task} · {body.value.task.slice(0,12)}</Button></>}
    {body?.kind==='observation'&&<><ReleaseFields value={body.value.release} t={t}/><p>{t.linkSampling}</p><p>{t.linkWindow}: {new Date(Number(body.value.start_unix_ms)).toLocaleString()} — {new Date(Number(body.value.end_unix_ms)).toLocaleString()}</p><p>{t.linkCollected}: {new Date(Number(body.value.collected_unix_ms)).toLocaleString()}</p><dl className="record-fields"><div><dt>{t.linkEvents}</dt><dd>{body.value.persisted_events}</dd></div><div><dt>{t.linkErrors}</dt><dd>{body.value.persisted_error_events}</dd></div><div><dt>{t.linkSessions}</dt><dd>{body.value.persisted_sessions}</dd></div></dl><p><code>{body.value.mapping_event_digest}</code></p></>}
    {summary.can_reauthorize&&<Button disabled={busy} onClick={async()=>{setBusy(true);setFailure(undefined);try{await api.postEmpty(`${base}/channels/${encodeURIComponent(summary.channel)}/events/${summary.digest}/reauthorize`);refresh();}catch(e){setFailure(e);}finally{setBusy(false);}}}>{t.linkResume}</Button>}
    {failure!==undefined&&<p role="alert" className="failure">{errorText(failure,t)}</p>}
    <details><summary>{t.linkOriginal}</summary><pre className="source"><code>{JSON.stringify(history,null,2)}</code></pre></details>
  </>}</section>;
}
function ReleaseFields({value,t}:{value:Release;t:Text}) {return <dl className="record-fields">{([[t.linkApplication,value.application],[t.linkReleaseId,value.release],[t.linkReleaseName,value.release_name],[t.linkEnvironment,value.environment||t.linkEmptyEnv],[t.linkArtifact,value.artifact_sha256]] as const).map(([label,text])=><div key={label}><dt>{label}</dt><dd><code>{text}</code></dd></div>)}</dl>;}
function kind(value:LinkSummary['kind'],t:Text){return {mapping:t.deliveryLinks,withdraw:t.linkWithdraw,repair_request:t.linkRepair,task_bound:t.linkTaskBound,observation:t.linkObservation}[value];}
function issueUrl(origin:string,issue:string,sample?:string) {try{const u=new URL(origin);if(!['https:','http:'].includes(u.protocol)||u.origin!==origin)return;u.pathname=`/issues/${encodeURIComponent(issue)}`;if(sample)u.searchParams.set('occurrence',sample);return u.href;}catch{return undefined;}}

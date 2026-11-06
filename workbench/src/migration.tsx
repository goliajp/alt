import { useEffect, useRef, useState } from 'react';
import { Button, Field, Input, NativeSelect } from '@golia/ui/controls';
import { api, ApiError, repoPath, type Repository, type RequestKey, type Scope, type Session } from './api';
import type { Text } from './i18n';
import { Failure, Loading, errorText, useResource } from './resource';
import { contentSlice, HEADER_BUDGET, readHeader, sameScope, selectArchive, type ArchiveHeader, type Capabilities, type Inspected, type Mapping, type Selection } from './archive';
import { journalId, journals, ownerKey, remember, type Cursor, type ExportState, type ImportIntent, type ImportState, type Journal } from './journal';
const hex=/^[0-9a-f]{64}$/;
function scoped(scope:Scope) {return `${repoPath(scope.repository_instance_id)}/epochs/${scope.recovery_epoch}/portable`;}
function exportPath(key:RequestKey) {return `${scoped(key.scope)}/exports/${key.request_id}`;}
function importPath(key:RequestKey) {return `${scoped(key.scope)}/imports/${key.actor_id}/${key.request_id}`;}
function newKey(scope:Scope,actor:string):RequestKey {return {scope,actor_id:actor,request_id:crypto.randomUUID().replaceAll('-','')};}
function sameKey(a:RequestKey,b:RequestKey) {return sameScope(a.scope,b.scope)&&a.actor_id===b.actor_id&&a.request_id===b.request_id;}
function sameIntent(a:ImportIntent,b:ImportIntent) {
  return a.version===b.version && sameKey(a.key,b.key) && a.target_public_key===b.target_public_key
    && sameScope(a.source,b.source) && a.source_public_key===b.source_public_key && a.catalog_digest===b.catalog_digest
    && a.mappings.length===b.mappings.length && a.mappings.every((v,i)=>v.installation_id===b.mappings[i].installation_id&&v.actor_id===b.mappings[i].actor_id&&v.target_actor===b.mappings[i].target_actor);
}
function checkExport(value:ExportState,request:Journal) {
  const source=value.state==='available'?value.envelope.catalog.source:value.scope;
  const id=value.state==='available'?value.envelope.catalog.export_id:value.export_id;
  if(!sameScope(source,request.key.scope)||id!==request.key.request_id) throw new Error('migration_response_identity');
  if(value.state==='available'&&value.envelope.catalog.source_public_key!==request.public_key) throw new Error('migration_response_identity');
  return value;
}
function checkImport(value:ImportState,request:Journal) {
  if(!request.intent) throw new Error('migration_journal_changed');
  if(value.state==='committed'&&!sameIntent(value.receipt.body.intent,request.intent)) throw new Error('migration_response_identity');
  if((value.state==='unknown'||value.state==='not_seen')&&!sameKey(value.key,request.key)) throw new Error('migration_response_identity');
  return value;
}
function migrationError(error:unknown,t:Text) {
  if(error instanceof ApiError)return errorText(error,t);
  const code=error instanceof Error?error.message:'';
  if(code==='migration_source_key')return t.migrationSourceKeyError;
  if(code.startsWith('archive_')||error instanceof SyntaxError||error instanceof TypeError)return t.migrationArchiveError;
  if(code.startsWith('migration_journal_'))return t.migrationJournalError;
  if(code==='migration_response_identity'||code==='migration_binding_changed')return t.migrationIdentityError;
  return errorText(error,t);
}
function RequestPicker({items,current,pick,t,disabled}:{items:Journal[];current:Journal|undefined;pick:(v:Journal)=>void;t:Text;disabled:boolean}) {
  if(!items.length) return null;
  return <Field label={t.migrationRequest}><NativeSelect disabled={disabled} value={current?.id??''} onChange={event=>{const value=items.find(v=>v.id===event.target.value);if(value)pick(value);}}>
    <option value="" disabled>{t.chooseMigrationRequest}</option>
    {items.map(v=><option value={v.id} key={v.id}>{new Date(v.created).toLocaleString()} · {v.key.request_id.slice(0,10)}</option>)}
  </NativeSelect></Field>;
}
export function MigrationView(props:{repository:string;session:Session;t:Text;log:(v:string)=>void}) {
  if(!props.session.user.administrator) return <p className="message">{props.t.migrationOwnerRequired}</p>;
  return <Migration {...props}/>;
}
function Migration({repository,session,t,log}:{repository:string;session:Session;t:Text;log:(v:string)=>void}) {
  const [revision,refresh]=useState(0);
  const info=useResource<Repository&{refs:Record<string,string>}>(repoPath(repository),revision);
  const caps=useResource<Capabilities>('/web/portable/capabilities',revision);
  const owner=ownerKey(session.installation_id,session.user.actor_id,repository);
  const [items,setItems]=useState<Journal[]>([]),[cursor,setCursor]=useState<Cursor|null>(null);
  const [source,setSource]=useState<Journal>(),[target,setTarget]=useState<Journal>();
  const [sourceState,setSourceState]=useState<ExportState>(),[targetState,setTargetState]=useState<ImportState>();
  const [busy,setBusy]=useState(''),[error,setError]=useState<unknown>();
  const running=useRef(false);
  const focusAfter=useRef<{control:HTMLElement;heading:HTMLElement|null}|null>(null);
  useEffect(()=>{
    if(busy)return;
    const previous=focusAfter.current;focusAfter.current=null;
    if(previous&&document.activeElement===document.body){
      const destination=previous.control.isConnected?previous.control:previous.heading;
      if(destination?.isConnected)destination.focus();
    }
  },[busy]);
  const [file,setFile]=useState<File>(),[header,setHeader]=useState<ArchiveHeader>(),[selection,setSelection]=useState<Selection>();
  const [sourceKey,setSourceKey]=useState(''),[mapping,setMapping]=useState<Mapping[]>([]),[mapped,setMapped]=useState(false),[uploaded,setUploaded]=useState({done:0,total:0});
  useEffect(()=>{let active=true;journals(owner).then(page=>{if(active){setItems(page.items);setCursor(page.next);}},e=>{if(active)setError(e);});return()=>{active=false;};},[owner]);
  const bind=(request:Journal)=>{
    if(!caps.value||request.owner!==owner||request.public_key!==caps.value.receipt_public_key||request.key.actor_id!==session.user.actor_id||request.key.scope.installation_id!==session.installation_id) throw new Error('migration_binding_changed');
  };
  const run=async(name:string,action:()=>Promise<void>)=>{if(running.current)return;running.current=true;const control=document.activeElement;if(control instanceof HTMLElement)focusAfter.current={control,heading:control.closest('.migration-panel')?.querySelector<HTMLElement>('h2')??null};setBusy(name);setError(undefined);try{await action();}catch(e){setError(e);}finally{running.current=false;setBusy('');}};
  const observeSource=async(request:Journal)=>{bind(request);setSourceState(checkExport(await api.portable<ExportState>(exportPath(request.key),'GET'),request));};
  const observeTarget=async(request:Journal)=>{bind(request);setTargetState(checkImport(await api.portable<ImportState>(importPath(request.key),'GET'),request));};
  const save=async(request:Journal)=>{await remember(request);setItems(current=>[request,...current.filter(v=>v.id!==request.id)]);};
  const sourceAction=async(action:''|'freeze'|'reopen'|'retire')=>{
    if(!source)return;bind(source);
    const value=checkExport(await api.portable<ExportState>(`${exportPath(source.key)}${action?`/${action}`:''}`,'POST'),source);
    setSourceState(value);log(t.migrationStateUpdated);
  };
  const stage=async()=>{
    if(!target?.intent||!file||!selection)return;bind(target);
    if(target.intent.catalog_digest!==selection.verified.catalog_digest||!sameScope(target.intent.source,selection.verified.envelope.catalog.source)||target.intent.source_public_key!==selection.verified.envelope.catalog.source_public_key) throw new Error('archive_changed');
    let value=checkImport(await api.portable<ImportState>(importPath(target.key),'POST',{intent:target.intent,envelope:selection.verified.envelope}),target);
    setTargetState(value);
    if(value.state==='receiving') {
      setUploaded({done:0,total:value.missing.length});let done=0;
      for(const blob of value.missing){await api.portable(`${importPath(target.key)}/content/${blob.blake3}`,'PUT',contentSlice(file,selection,blob));setUploaded({done:++done,total:value.missing.length});}
      value=checkImport(await api.portable<ImportState>(importPath(target.key),'GET'),target);setTargetState(value);
    }
    if(value.state==='receiving'&&value.missing.length===0)log(t.importStaged);
    if(value.state==='committed')log(t.importAccepted);
  };
  const targetAction=async(action:'activate'|'abandon')=>{
    if(!target?.intent)return;bind(target);
    const value=checkImport(await api.portable<ImportState>(`${importPath(target.key)}/${action}`,'POST',action==='activate'?target.intent:undefined),target);setTargetState(value);log(t.migrationStateUpdated);
  };
  const choose=async(chosen:File)=>{
    setFile(chosen);setHeader(undefined);setSelection(undefined);setMapped(false);setMapping([]);
    const header=await readHeader(chosen,Math.min(HEADER_BUDGET,caps.value?.bounds.metadata_bytes??HEADER_BUDGET));setHeader(header);
  };
  const inspect=async()=>{
    if(!header||!file||!info.value||!caps.value)return;
    if(!hex.test(sourceKey))throw new Error('migration_source_key');
    const checked=await api.portable<Inspected>(`${scoped(info.value.scope)}/inspect`,'POST',{source:header.hint.catalog.source,source_public_key:sourceKey,metadata:header.metadata});
    if(checked.envelope.catalog.source_public_key!==sourceKey||!sameScope(checked.envelope.catalog.source,header.hint.catalog.source))throw new Error('migration_response_identity');
    const selected=selectArchive(header,checked,caps.value.bounds);
    if(target?.intent) {
      if(target.intent.catalog_digest!==checked.catalog_digest||!sameScope(target.intent.source,checked.envelope.catalog.source))throw new Error('archive_changed');
      setMapping(target.intent.mappings);setMapped(true);
    } else {setMapping(checked.envelope.catalog.identities.map(v=>({installation_id:v.installation_id,actor_id:v.actor_id,target_actor:null})));setMapped(false);}
    setSelection(selected);
  };
  const createImport=async()=>{
    if(!selection||!info.value||!caps.value||!mapped)return;
    const key=newKey(info.value.scope,session.user.actor_id);
    const intent:ImportIntent={version:1,key,target_public_key:caps.value.receipt_public_key,source:selection.verified.envelope.catalog.source,source_public_key:sourceKey,catalog_digest:selection.verified.catalog_digest,mappings:mapping};
    const request:Journal={version:1,id:journalId(owner,key.request_id),owner,created:Date.now(),kind:'import',key,public_key:caps.value.receipt_public_key,intent};
    await save(request);setTarget(request);setTargetState(undefined);log(t.migrationRequestSaved);
  };
  const selectedSource=(request:Journal)=>{setSource(request);setSourceState(undefined);void run('query',()=>observeSource(request));};
  const selectedTarget=(request:Journal)=>{setTarget(request);setTargetState(undefined);setSelection(undefined);setMapping(request.intent?.mappings??[]);setMapped(false);setSourceKey(request.intent?.source_public_key??'');void run('query',()=>observeTarget(request));};
  if(caps.error instanceof ApiError&&caps.error.status===409)return <p role="status">{t.migrationUnavailable}</p>;
  if(info.error||caps.error)return <Failure error={info.error??caps.error} t={t} retry={()=>refresh(v=>v+1)}/>;
  if(!info.value||!caps.value)return <Loading t={t}/>;
  if(caps.value.version!==1||caps.value.installation_id!==session.installation_id||!hex.test(caps.value.receipt_public_key))return <p role="alert" className="failure">{t.migrationIdentityError}</p>;
  const sourceChoices=items.filter(v=>v.kind==='export'),targetChoices=items.filter(v=>v.kind==='import');
  return <><header className="document-heading"><h1>{repository} · {t.migration}</h1><Button disabled={!!busy} onClick={()=>refresh(v=>v+1)}>{t.refresh}</Button></header>
    <p>{t.migrationIntro}</p>{error&&<div role="alert"><p className="failure">{migrationError(error,t)}</p><p>{t.migrationQueryFirst}</p></div>}
    <div className="migration-grid"><section className="migration-panel"><h2 tabIndex={-1}>{t.exportSnapshot}</h2><p>{t.exportSnapshotHint}</p>
      <Field label={t.sourceVerificationKey}><Input readOnly value={caps.value.receipt_public_key}/></Field>
      <div className="document-actions"><Button disabled={!!busy} onClick={()=>void run('export',async()=>{
        const key=newKey(info.value!.scope,session.user.actor_id);
        const request:Journal={version:1,id:journalId(owner,key.request_id),owner,created:Date.now(),kind:'export',key,public_key:caps.value!.receipt_public_key};
        await save(request);setSource(request);setSourceState(undefined);
        setSourceState(checkExport(await api.portable<ExportState>(exportPath(key),'POST'),request));log(t.migrationStateUpdated);
      })}>{t.newSnapshot}</Button></div>
      <RequestPicker items={sourceChoices} current={source} pick={selectedSource} t={t} disabled={!!busy}/>
      {source&&<><code className="migration-id">{source.key.request_id}</code><div className="document-actions"><Button disabled={!!busy} onClick={()=>void run('query',()=>observeSource(source))}>{t.queryOriginalRequest}</Button><Button disabled={!!busy} onClick={()=>void run('export',()=>sourceAction(''))}>{t.resumeSnapshot}</Button></div></>}
      {sourceState&&<p role="status">{t[`migration_${sourceState.state}`]}{sourceState.state==='available'&&` · ${sourceState.frozen?t.sourceFrozen:t.sourceNotFrozen}`}</p>}
      {source&&sourceState?.state==='available'&&<><dl className="record-fields"><div><dt>{t.snapshotWatermark}</dt><dd>{sourceState.envelope.catalog.snapshot_sequence}</dd></div><div><dt>{t.repository}</dt><dd>{sourceState.envelope.catalog.source.repository_instance_id}</dd></div></dl>
        <a href={`${exportPath(source.key)}/archive`}>{t.downloadArchive}</a><p>{t.sourceCutoverHint}</p><div className="document-actions">
          <Button disabled={!!busy||sourceState.frozen} onClick={()=>void run('freeze',()=>sourceAction('freeze'))}>{t.freezeSource}</Button>
          <Button disabled={!!busy||!sourceState.frozen} onClick={()=>void run('reopen',()=>sourceAction('reopen'))}>{t.reopenSource}</Button>
        </div></>}
      {source&&sourceState&&sourceState.state!=='retired'&&<Button disabled={!!busy||(sourceState.state==='available'&&sourceState.frozen)} onClick={()=>void run('retire',()=>sourceAction('retire'))}>{t.retireSnapshot}</Button>}
    </section><section className="migration-panel"><h2 tabIndex={-1}>{t.importSnapshot}</h2><p>{t.importSnapshotHint}</p>
      <RequestPicker items={targetChoices} current={target} pick={selectedTarget} t={t} disabled={!!busy}/>
      {target&&<><code className="migration-id">{target.key.request_id}</code><Button disabled={!!busy} onClick={()=>void run('query',()=>observeTarget(target))}>{t.queryOriginalRequest}</Button></>}
      {targetState&&<p role="status">{t[`migration_${targetState.state}`]}{targetState.state==='receiving'&&` · ${t.remainingContent}: ${targetState.missing.length} · ${t.importExpiry}: ${new Date(Number(targetState.expires_at_unix_ms)).toLocaleString()}`}</p>}
      {targetState?.state==='committed'?<><p className="success">{t.importAccepted}</p><dl className="record-fields"><div><dt>{t.sourceInstance}</dt><dd>{targetState.receipt.body.intent.source.installation_id}</dd></div><div><dt>{t.targetReceiptSequence}</dt><dd>{targetState.receipt.body.transaction_sequence}</dd></div></dl><p>{t.targetAcceptedHint}</p></>:<>
        <Field label={t.archiveFile}><Input type="file" accept=".altportable,application/vnd.alt.portable-v1" disabled={!!busy} onChange={event=>{const file=event.target.files?.[0];if(file)void run('file',()=>choose(file));}}/></Field>
        <Field label={t.trustedSourceKey}><Input value={sourceKey} maxLength={64} disabled={!!busy||!!target} onChange={event=>{setSourceKey(event.target.value);setSelection(undefined);setMapped(false);}}/></Field><p>{t.trustedSourceKeyHint}</p>
        <Button disabled={!!busy||!header||!hex.test(sourceKey)} onClick={()=>void run('inspect',inspect)}>{t.inspectArchive}</Button>
        {selection&&<><dl className="record-fields"><div><dt>{t.sourceInstance}</dt><dd>{selection.verified.envelope.catalog.source.installation_id}</dd></div><div><dt>{t.repository}</dt><dd>{selection.verified.envelope.catalog.source.repository_instance_id}</dd></div><div><dt>{t.snapshotWatermark}</dt><dd>{selection.verified.envelope.catalog.snapshot_sequence}</dd></div><div><dt>{t.archiveBytes}</dt><dd>{file?.size.toLocaleString()}</dd></div></dl>
          <p>{t.inspectedArchiveHint}</p><h3>{t.identityDisposition}</h3><p>{t.identityDispositionHint}</p>
          <div className="migration-mappings">{mapping.map((row,index)=><Field key={`${row.installation_id}/${row.actor_id}`} label={`${row.installation_id} / ${row.actor_id}`}><Input value={row.target_actor??''} placeholder={t.historyOnly} disabled={!!busy||!!target} maxLength={128} onChange={event=>{const value=event.target.value;setMapping(current=>current.map((v,i)=>i===index?{...v,target_actor:value||null}:v));setMapped(false);}}/></Field>)}</div>
          {!target&&<label className="migration-confirm"><input type="checkbox" disabled={!!busy} checked={mapped} onChange={event=>setMapped(event.target.checked)}/>{t.confirmIdentityDisposition}</label>}
          {!target&&<Button disabled={!!busy||!mapped||Object.keys(info.value.refs).length>0} onClick={()=>void run('journal',createImport)}>{t.saveImportRequest}</Button>}
          {target&&<Button disabled={!!busy||targetState?.state==='abandoned'||targetState?.state==='unknown'} onClick={()=>void run('upload',stage)}>{t.resumeImportUpload}</Button>}
          {!!uploaded.total&&<div role="status"><progress max={uploaded.total} value={uploaded.done}/><span>{t.uploadedContent}: {uploaded.done} / {uploaded.total}</span></div>}
        </>}
      </>}
      {target&&targetState?.state==='receiving'&&<><p>{t.targetActivationHint}</p><div className="document-actions"><Button variant="primary" disabled={!!busy||targetState.missing.length>0} onClick={()=>void run('activate',()=>targetAction('activate'))}>{t.activateImport}</Button><Button disabled={!!busy} onClick={()=>void run('abandon',()=>targetAction('abandon'))}>{t.abandonImport}</Button></div></>}
      {target&&<Button disabled={!!busy} onClick={()=>{setTarget(undefined);setTargetState(undefined);setSelection(undefined);setMapping([]);setMapped(false);setSourceKey('');}}>{t.newImportRequest}</Button>}
    </section></div>
    {cursor&&<Button disabled={!!busy} onClick={()=>void run('history',async()=>{const page=await journals(owner,cursor);setItems(current=>[...current,...page.items.filter(v=>!current.some(old=>old.id===v.id))]);setCursor(page.next);})}>{t.olderMigrationRequests}</Button>}
  </>;
}

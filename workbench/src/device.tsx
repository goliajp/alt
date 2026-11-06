import {useRef,useState} from 'react';
import {Button,Field,Input} from '@golia/ui/controls';
import {api,type Session} from './api';
import type {Text} from './i18n';
import {Failure,Loading,errorText,useResource} from './resource';
import {normalizeDeviceCode,forgetDeviceEntry} from './device-location';

type Consent={id:string;client_id:'alt-cli';scope:'account';state:'pending'|'approved'|'denied';expires_at_unix_ms:number};
export function DeviceAuthorization({code,session,t,close}:{code:string;session:Session;t:Text;close:()=>void}) {
  const [entered,setEntered]=useState(code),[selected,select]=useState(normalizeDeviceCode(code));
  return <main className="login"><h1>{t.deviceTitle}</h1><p>{t.account}: <strong>{session.user.display_name}</strong></p><p>{t.instance}: <span className="device-origin">{location.origin}</span></p>
    {selected?<ConsentView key={selected} code={selected} t={t}/>:<form onSubmit={event=>{event.preventDefault();const normalized=normalizeDeviceCode(entered);if(normalized)select(normalized);}}>
      <p>{t.deviceEnter}</p><Field label={t.deviceCode}><Input value={entered} onChange={event=>setEntered(event.target.value)} autoComplete="off" autoCapitalize="characters" maxLength={32} required/></Field>
      <Button type="submit" disabled={!normalizeDeviceCode(entered)}>{t.confirm}</Button>
    </form>}
    <p><Button onClick={()=>{forgetDeviceEntry();close();}}>{t.back}</Button></p>
  </main>;
}
function ConsentView({code,t}:{code:string;t:Text}) {
  const [revision,refresh]=useState(0),[busy,setBusy]=useState(false),[failure,setFailure]=useState<unknown>();
  const locked=useRef(false);
  const resource=useResource<Consent>('/web/device?user_code='+encodeURIComponent(code),revision);
  const reload=()=>{setFailure(undefined);refresh(value=>value+1);};
  async function decide(allow:boolean){
    if(locked.current||!resource.value||resource.value.state!=='pending')return;
    locked.current=true;setBusy(true);setFailure(undefined);
    try{await api.request('/web/device',{user_code:code,allow});forgetDeviceEntry();refresh(value=>value+1);}
    catch(error){setFailure(error);}
    finally{locked.current=false;setBusy(false);}
  }
  return <><p>{t.deviceMatch}</p><p className="device-code" aria-label={t.deviceCode}>{code}</p>
    {resource.error?<Failure error={resource.error} t={t} retry={reload}/>:!resource.value?<Loading t={t}/>:resource.value.state==='pending'?<>
      <p>{t.deviceScope}</p><p>{t.deviceLifetime}</p><p>{t.expiresAt}: {new Date(resource.value.expires_at_unix_ms).toLocaleString()}</p>
      <div className="document-actions"><Button variant="primary" busy={busy} disabled={failure!==undefined} onClick={()=>void decide(true)}>{t.deviceAllow}</Button><Button disabled={busy||failure!==undefined} onClick={()=>void decide(false)}>{t.deviceDeny}</Button></div>
    </>:<p role="status">{resource.value.state==='approved'?t.deviceApproved:t.deviceDenied}</p>}
    {failure!==undefined&&<><p role="alert" className="failure">{errorText(failure,t)}</p><Button onClick={reload}>{t.refresh}</Button></>}
  </>;
}

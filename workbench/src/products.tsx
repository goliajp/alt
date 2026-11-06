import { useEffect, useState } from 'react';
import { Button, NativeSelect } from '@golia/ui/controls';
import { Dialog } from '@golia/ui/Dialog';
import { type Text } from './i18n';
import { useResource } from './resource';

type Navigation={workspace_id:string;workspace_name:string;control_url:string;products:{product:'alt'|'sentori';installation_id:string;name:string;url:string}[]};
function destination(value:string,product:'alt'|'sentori') {
  try {const url=new URL(value);return !url.username&&!url.password&&!url.search&&!url.hash&&url.pathname===(product==='alt'?'/web/login/start':'/auth/cloud/start')&&(url.protocol==='https:'||(url.protocol==='http:'&&['localhost','127.0.0.1','[::1]'].includes(url.hostname)))?url.href:null;}catch{return null;}
}
export function CloudProducts({t,workspace}:{t:Text;workspace:string}) {
  const [revision,reload]=useState(0),[pending,setPending]=useState<{url:string;name:string}|null>(null);
  const nav=useResource<Navigation>('/web/navigation',revision);
  useEffect(()=>{const refresh=()=>reload(value=>value+1),timer=window.setInterval(refresh,30000);window.addEventListener('focus',refresh);return()=>{window.clearInterval(timer);window.removeEventListener('focus',refresh);};},[]);
  const value=nav.error?null:nav.value;
  return <><NativeSelect aria-label={t.product} value="alt" onChange={event=>{
    const product=value?.products.find(item=>item.product===event.target.value);if(!product)return;
    const url=destination(product.url,product.product);if(url)setPending({url,name:product.product==='alt'?'alt':'Sentori'});
  }}><option value="alt">alt</option>{value?.products.filter(item=>item.product!=='alt').map(item=><option key={item.installation_id} value={item.product}>{item.product==='alt'?'alt':'Sentori'} · {item.name}</option>)}</NativeSelect>
    <span className="workspace" title={t.workspace}>{value?.workspace_name??workspace}</span>
    {value&&<a href={value.control_url} target="_blank" rel="noopener noreferrer">{t.cloudAccount}</a>}
    {nav.error&&<Button small title={t.productsUnavailable} onClick={()=>reload(value=>value+1)}>{t.retryProducts}</Button>}
    <Dialog open={pending!==null} onClose={()=>setPending(null)} title={t.switchProduct} closeLabel={t.close} footer={<><Button onClick={()=>setPending(null)}>{t.close}</Button><Button variant="primary" onClick={()=>pending&&window.location.assign(pending.url)}>{t.continueProduct}</Button></>}><p>{pending?.name}</p><p>{t.switchProductHint}</p></Dialog>
  </>;
}

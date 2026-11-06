export function normalizeDeviceCode(value:string):string|null {
  if(!/^[\x00-\x7f]*$/.test(value.trim()))return null;
  const input=value.trim().toUpperCase();
  const raw=input.length===9&&input[4]==='-'?input.slice(0,4)+input.slice(5):input;
  return /^[0123456789ABCDEFGHJKMNPQRSTVWXYZ]{8}$/.test(raw)?raw.slice(0,4)+'-'+raw.slice(4):null;
}
export function deviceFragment(hash:string):{code:string}|null {
  if(hash==='#device')return {code:''};
  if(!hash.startsWith('#device?'))return null;
  const params=new URLSearchParams(hash.slice('#device?'.length));
  if([...params.keys()].some(key=>key!=='user_code')||params.getAll('user_code').length!==1)return {code:''};
  return {code:normalizeDeviceCode(params.get('user_code')??'')??''};
}
const key='alt.device.consent';
// Only a human-readable code survives a Cloud navigation, never a device code
// or bearer. Returning from authentication still requires explicit consent.
export function captureDeviceEntry():{code:string}|null {
  const entry=deviceFragment(location.hash);
  try {
    if(entry){sessionStorage.setItem(key,JSON.stringify({...entry,expires:Date.now()+300000}));return entry;}
    if(!location.hash){
      const saved=JSON.parse(sessionStorage.getItem(key)??'null') as {code?:unknown;expires?:unknown}|null;
      if(saved&&typeof saved.code==='string'&&typeof saved.expires==='number'&&saved.expires>Date.now()&&saved.expires<=Date.now()+300000){
        const code=normalizeDeviceCode(saved.code)??'';
        history.replaceState(null,'',location.pathname+location.search+'#device'+(code?'?user_code='+code:''));
        return {code};
      }
      sessionStorage.removeItem(key);
    }
  }catch{/* The current fragment works when storage is disabled. */}
  return entry;
}
export function forgetDeviceEntry(){try{sessionStorage.removeItem(key);}catch{/* No persisted navigation. */}}

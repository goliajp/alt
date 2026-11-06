import type { RequestKey } from './api';
import type { MappingRequest, WithdrawalRequest } from './change-link-model';
export type Journal={version:1;id:string;owner:string;created:number;key:RequestKey;kind:'mapping'|'withdraw';intent:MappingRequest|WithdrawalRequest};
const DATABASE='alt-change-link-journals-v1';
function open():Promise<IDBDatabase> {
  return new Promise((resolve,reject)=>{
    const request=indexedDB.open(DATABASE,1);
    request.onupgradeneeded=()=>{
      const db=request.result;
      const store=db.createObjectStore('requests',{keyPath:'id'});
      store.createIndex('owner_created',['owner','created','id']);
    };
    request.onsuccess=()=>{request.result.onversionchange=()=>request.result.close();resolve(request.result);};
    request.onerror=()=>reject(new Error('change_journal_unavailable'));
    request.onblocked=()=>reject(new Error('change_journal_blocked'));
  });
}
export function ownerKey(installation:string,actor:string,repository:string) {return JSON.stringify([installation,actor,repository]);}
export function journalId(owner:string,request:string) {return JSON.stringify([owner,request]);}
/** Complete the IndexedDB transaction before sending any mutation. Reusing an
 * ID never replaces its original scope, mapping or source identity. */
export async function remember(journal:Journal):Promise<void> {
  const db=await open();
  try {
    await new Promise<void>((resolve,reject)=>{
      const tx=db.transaction('requests','readwrite',{durability:'strict'});
      const store=tx.objectStore('requests');
      const request=store.get(journal.id);
      let conflict=false;
      request.onsuccess=()=>{
        if (request.result!==undefined) {
          if (JSON.stringify(request.result)!==JSON.stringify(journal)) {conflict=true;tx.abort();}
        } else {store.add(journal);}
      };
      tx.oncomplete=()=>resolve();
      tx.onerror=tx.onabort=()=>reject(new Error(conflict?'change_journal_changed':'change_journal_unavailable'));
    });
  } finally {db.close();}
}
export type Cursor={created:number;id:string};
export async function journals(owner:string,before?:Cursor):Promise<{items:Journal[];next:Cursor|null}> {
  const db=await open();
  try {
    return await new Promise((resolve,reject)=>{
      const tx=db.transaction('requests','readonly');
      const upper=before?[owner,before.created,before.id]:[owner,Number.MAX_SAFE_INTEGER,'\uffff'];
      const range=IDBKeyRange.bound([owner,0,''],upper,false,!!before);
      const request=tx.objectStore('requests').index('owner_created').openCursor(range,'prev');
      const items:Journal[]=[];
      let more=false;
      request.onsuccess=()=>{
        const cursor=request.result;
        if (!cursor) return;
        if(items.length===32){more=true;return;}
        const row=cursor.value as Journal;
        if(row.version!==1 || row.owner!==owner || row.id!==journalId(owner,row.key.request_id)) {tx.abort();return;}
        items.push(row);cursor.continue();
      };
      tx.oncomplete=()=>{const last=items.at(-1);resolve({items,next:more&&last?{created:last.created,id:last.id}:null});};
      tx.onerror=tx.onabort=()=>reject(new Error('change_journal_unavailable'));
    });
  } finally {db.close();}
}

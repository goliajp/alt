import type { RequestKey, Scope } from './api';
import type { Envelope, Mapping } from './archive';
export type ImportIntent = {version:1;key:RequestKey;target_public_key:string;source:Scope;source_public_key:string;catalog_digest:string;mappings:Mapping[]};
export type ImportReceipt = {body:{version:1;intent:ImportIntent;transaction_sequence:string;committed_at_unix_ms:string};signature:string};
export type ImportState =
  | {state:'receiving';digest:string;expires_at_unix_ms:string;missing:{blake3:string;bytes:string}[]}
  | {state:'committed';receipt:ImportReceipt}
  | {state:'abandoned';digest:string}
  | {state:'unknown'|'not_seen';key:RequestKey};
export type ExportState =
  | {state:'available';envelope:Envelope;frozen:boolean}
  | {state:'preparing';scope:Scope;export_id:string;catalog_digest:string;snapshot_sequence:string}
  | {state:'retired';scope:Scope;export_id:string;catalog_digest:string}
  | {state:'unknown'|'not_seen';scope:Scope;export_id:string};
export type Journal = {
  version:1; id:string; owner:string; created:number; kind:'export'|'import'; key:RequestKey;
  public_key:string; intent?:ImportIntent;
};
const DATABASE='alt-portable-journals-v1';
function open():Promise<IDBDatabase> {
  return new Promise((resolve,reject)=>{
    const request=indexedDB.open(DATABASE,1);
    request.onupgradeneeded=()=>{
      const db=request.result;
      const store=db.createObjectStore('requests',{keyPath:'id'});
      store.createIndex('owner_created',['owner','created','id']);
    };
    request.onsuccess=()=>{request.result.onversionchange=()=>request.result.close();resolve(request.result);};
    request.onerror=()=>reject(new Error('migration_journal_unavailable'));
    request.onblocked=()=>reject(new Error('migration_journal_blocked'));
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
      tx.onerror=tx.onabort=()=>reject(new Error(conflict?'migration_journal_changed':'migration_journal_unavailable'));
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
      tx.onerror=tx.onabort=()=>reject(new Error('migration_journal_unavailable'));
    });
  } finally {db.close();}
}

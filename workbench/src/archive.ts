import type { Mode, Scope } from './api';
export type PortableBlob = { blake3: string; bytes: string };
export type Identity = { installation_id: string; actor_id: string; had_read: boolean; had_publish_records: boolean; had_review: boolean; ref_grants: unknown[] };
export type Mapping = { installation_id: string; actor_id: string; target_actor: string | null };
export type Envelope = { catalog: {
  version: number; source: Scope; source_mode: Mode; source_public_key: string; export_id: string;
  snapshot_sequence: string; refs: {name:string;oid:string}[]; identities: Identity[]; blobs: PortableBlob[];
  publications: unknown[];
}; signature: string };
export type Inspected = { envelope: Envelope; catalog_digest: string };
export type Bounds = {metadata_bytes:number;entries:number;publications:number;blob_bytes:string;total_bytes:string;work_bytes:string;reads:number;import_ttl_ms:string};
export type Capabilities = {version:1;installation_id:string;receipt_public_key:string;bounds:Bounds};
export type ArchiveHeader = {metadata:string; hint:Envelope; end:number; size:number; footer:string};
export type Selection = {header:ArchiveHeader; verified:Inspected; files:Map<string,{blob:PortableBlob;offset:number}>};
const encoder = new TextEncoder();
const MAGIC = encoder.encode('ALT-PORTABLE-V1\0');
const COMPLETE = encoder.encode('ALT-COMPLETE-V1\0');
export const HEADER_BUDGET = 16 * 1024 * 1024;
const U64_MAX = (1n << 64n) - 1n;
export function decimal(value: unknown): bigint {
  if (typeof value !== 'string' || !/^(0|[1-9][0-9]*)$/.test(value)) throw new Error('archive_integer');
  const result = BigInt(value); if (result > U64_MAX) throw new Error('archive_integer'); return result;
}
export function sameScope(a: Scope, b: Scope) {
  return a.installation_id === b.installation_id && a.repository_instance_id === b.repository_instance_id && a.recovery_epoch === b.recovery_epoch;
}
function equal(a: Uint8Array,b:Uint8Array) { return a.length===b.length && a.every((v,i)=>v===b[i]); }
function digest(bytes: Uint8Array) { return Array.from(bytes,v=>v.toString(16).padStart(2,'0')).join(''); }
/** Only bounded header/footer reads occur here. The destination authenticates
 * the exact raw header before the UI treats any catalog declaration as trusted. */
export async function readHeader(file: globalThis.Blob, maximum=HEADER_BUDGET): Promise<ArchiveHeader> {
  if (!Number.isSafeInteger(file.size) || file.size<72) throw new Error('archive_incomplete');
  const prefix = new Uint8Array(await file.slice(0,24).arrayBuffer());
  if (prefix.length!==24 || !equal(prefix.slice(0,16),MAGIC)) throw new Error('archive_version');
  const count = new DataView(prefix.buffer,prefix.byteOffset,prefix.byteLength).getBigUint64(16,true);
  if (count===0n || count>BigInt(maximum) || count>BigInt(file.size-72)) throw new Error('archive_metadata_limit');
  const end=24+Number(count);
  const metadata=new TextDecoder('utf-8',{fatal:true}).decode(await file.slice(24,end).arrayBuffer());
  const hint:Envelope=JSON.parse(metadata);
  if (!hint || typeof hint!=='object' || !hint.catalog || typeof hint.catalog!=='object') throw new Error('archive_metadata');
  const source=hint.catalog.source;
  if(!source||typeof source!=='object'||typeof source.installation_id!=='string'||typeof source.repository_instance_id!=='string'
    ||!/^[A-Za-z0-9_-]{1,128}$/.test(source.installation_id)||!/^[A-Za-z0-9_-]{1,128}$/.test(source.repository_instance_id)
    ||decimal(source.recovery_epoch)===0n)throw new Error('archive_metadata');
  const footer=new Uint8Array(await file.slice(file.size-48).arrayBuffer());
  if (footer.length!==48 || !equal(footer.slice(0,16),COMPLETE)) throw new Error('archive_incomplete');
  return {metadata,hint,end,size:file.size,footer:digest(footer.slice(16))};
}
/** A completed header inspection does not validate payload bytes. The server
 * verifies each uploaded blob and the complete graph before target activation. */
export function selectArchive(header:ArchiveHeader, verified:Inspected, bounds:Bounds): Selection {
  if (!/^[0-9a-f]{64}$/.test(verified.catalog_digest) || verified.catalog_digest!==header.footer) throw new Error('archive_incomplete');
  const {blobs}=verified.envelope.catalog;
  if (!Array.isArray(blobs) || blobs.length>bounds.entries) throw new Error('archive_entry_limit');
  let offset=BigInt(header.end),total=0n,previous='';
  const maximum=decimal(bounds.blob_bytes), aggregate=decimal(bounds.total_bytes);
  const files=new Map<string,{blob:PortableBlob;offset:number}>();
  for (const blob of blobs) {
    if (!/^[0-9a-f]{64}$/.test(blob.blake3) || blob.blake3<=previous) throw new Error('archive_inventory');
    const count=decimal(blob.bytes); total+=count;
    if (count>maximum || total>aggregate || offset+count>BigInt(header.size-48)) throw new Error('archive_content_limit');
    files.set(blob.blake3,{blob,offset:Number(offset)});
    offset+=count; previous=blob.blake3;
  }
  if (offset!==BigInt(header.size-48)) throw new Error('archive_incomplete');
  return {header,verified,files};
}
export function contentSlice(file:globalThis.Blob, selection:Selection, blob:PortableBlob) {
  if (file.size!==selection.header.size) throw new Error('archive_changed');
  const selected=selection.files.get(blob.blake3);
  if (!selected || selected.blob.bytes!==blob.bytes) throw new Error('archive_selection');
  return file.slice(selected.offset,selected.offset+Number(decimal(blob.bytes)),'application/octet-stream');
}

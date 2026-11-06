import { expect, test } from 'bun:test';
import { contentSlice, decimal, readHeader, selectArchive, type Bounds, type Inspected } from './archive';
const encoder=new TextEncoder();
const bounds:Bounds={metadata_bytes:10000,entries:5,publications:5,blob_bytes:'10',total_bytes:'20',work_bytes:'100',reads:100,import_ttl_ms:'60000'};
function fixture() {
  // This fixture tests browser layout only. Signature and byte-digest checking
  // belong to the authenticated server and have separate Rust regressions.
  const verified:Inspected={catalog_digest:'f'.repeat(64),envelope:{signature:'0'.repeat(128),catalog:{version:1,source:{installation_id:'source',repository_instance_id:'repo',recovery_epoch:'9007199254740993'},source_mode:'self_hosted',source_public_key:'e'.repeat(64),export_id:'export',snapshot_sequence:'1',refs:[],publications:[],identities:[],blobs:[{blake3:'a'.repeat(64),bytes:'0'},{blake3:'b'.repeat(64),bytes:'3'}]}}};
  const metadata=encoder.encode(JSON.stringify(verified.envelope));
  const length=new Uint8Array(8);new DataView(length.buffer).setBigUint64(0,BigInt(metadata.length),true);
  const file=new Blob([encoder.encode('ALT-PORTABLE-V1\0'),length,metadata,encoder.encode('abc'),encoder.encode('ALT-COMPLETE-V1\0'),new Uint8Array(32).fill(255)]);
  return {file,verified};
}
test('bounded archive layout keeps exact large epochs and selects only signed content slices',async()=>{
  const {file,verified}=fixture();const header=await readHeader(file);
  expect(header.hint.catalog.source.recovery_epoch).toBe('9007199254740993');
  const selected=selectArchive(header,verified,bounds);
  expect(await contentSlice(file,selected,verified.envelope.catalog.blobs[0]).text()).toBe('');
  expect(await contentSlice(file,selected,verified.envelope.catalog.blobs[1]).text()).toBe('abc');
  expect(()=>contentSlice(file,selected,{blake3:'c'.repeat(64),bytes:'1'})).toThrow('archive_selection');
  expect(()=>contentSlice(file,selected,{blake3:'b'.repeat(64),bytes:'2'})).toThrow('archive_selection');
});
test('archive completion, metadata allocation and selected content budgets are checked before upload',async()=>{
  const {file,verified}=fixture();const header=await readHeader(file);
  await expect(readHeader(file.slice(0,file.size-1))).rejects.toThrow('archive_incomplete');
  await expect(readHeader(file,1)).rejects.toThrow('archive_metadata_limit');
  const raw=new Uint8Array(await file.arrayBuffer());new DataView(raw.buffer).setBigUint64(16,1n<<60n,true);
  await expect(readHeader(new Blob([raw]))).rejects.toThrow('archive_metadata_limit');
  expect(()=>selectArchive({...header,footer:'0'.repeat(64)},verified,bounds)).toThrow('archive_incomplete');
  expect(()=>selectArchive(header,verified,{...bounds,blob_bytes:'2'})).toThrow('archive_content_limit');
  expect(()=>selectArchive({...header,size:header.size+1},verified,bounds)).toThrow('archive_incomplete');
  expect(()=>decimal('01')).toThrow();expect(()=>decimal('18446744073709551616')).toThrow();
});

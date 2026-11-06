import type { RecordKind } from './api';
export type Document =
  | { kind: 'inbox' | 'repositories' | 'settings' }
  | { kind: 'repository' | 'history' | 'tasks' | 'deliveries' | 'migration'; repository: string }
  | { kind: 'tree'; repository: string; commit: string; path: string }
  | { kind: 'blob'; repository: string; oid: string; path: string }
  | { kind: 'record'; repository: string; recordKind: RecordKind; id: string }
  | { kind: 'review'; repository: string; actor: string; request: string; epoch: string };
export type OpenDocument = { key: string; document: Document };
// Length-prefixed components avoid delimiter collisions in repository/path IDs.
export function documentKey(document: Document) {
  return encodeURIComponent(Object.entries(document).sort(([a], [b]) => a.localeCompare(b)).map(([key, value]) => `${key.length}:${key}${String(value).length}:${value}`).join(''));
}
export function openDocument(documents: OpenDocument[], document: Document) {
  const key = documentKey(document);
  return { key, documents: documents.some(item => item.key === key) ? documents : [...documents, { key, document }] };
}
export function closeDocuments(documents: OpenDocument[], active: string, keys: string[]) {
  const index = documents.findIndex(item => item.key === active);
  const remaining = documents.filter(item => !keys.includes(item.key));
  const next = remaining.some(item => item.key === active) ? active : remaining[Math.min(index, remaining.length - 1)]?.key ?? '';
  return { documents: remaining, active: next };
}
export function moveDocument(documents: OpenDocument[], key: string, index: number) {
  const item = documents.find(item => item.key === key);
  if (!item || !Number.isInteger(index)) return documents;
  const next = documents.filter(item => item.key !== key);
  next.splice(Math.max(0, Math.min(next.length, index)), 0, item);
  return next;
}
export function belongsTo(document: Document, repository: string) { return 'repository' in document && document.repository === repository; }

// The address contains only document identity. A receiver must authenticate to
// the same installation; the server independently authorizes every read.
export function documentHash(document: Document) { return `#document=${encodeURIComponent(JSON.stringify(document))}`; }
export function documentFromHash(hash: string): Document | undefined {
  if (!hash.startsWith('#document=') || hash.length > 16384) return;
  let value: unknown;
  try { value = JSON.parse(decodeURIComponent(hash.slice(10))); } catch { return; }
  if (!value || typeof value !== 'object' || Array.isArray(value)) return;
  const data = value as Record<string, unknown>;
  const text = (key: string, max = 128) => typeof data[key] === 'string' && (data[key] as string).length <= max;
  const identity = (key: string) => text(key) && /^[A-Za-z0-9_-]+$/.test(data[key] as string);
  const oid = (key: string) => text(key, 40) && /^[0-9a-f]{40}$/.test(data[key] as string);
  const exact = (...keys: string[]) => Object.keys(data).sort().join(',') === keys.sort().join(',');
  if (['inbox', 'repositories', 'settings'].includes(String(data.kind)) && exact('kind')) return data as Document;
  if (!identity('repository')) return;
  if (['repository', 'history', 'tasks', 'deliveries', 'migration'].includes(String(data.kind)) && exact('kind', 'repository')) return data as Document;
  if (data.kind === 'tree' && oid('commit') && text('path', 8192) && /^(?:[0-9a-f]{2})*$/.test(data.path as string) && exact('kind', 'repository', 'commit', 'path')) return data as Document;
  if (data.kind === 'blob' && oid('oid') && text('path', 4096) && exact('kind', 'repository', 'oid', 'path')) return data as Document;
  if (data.kind === 'record' && ['task', 'candidate', 'evidence', 'review', 'integration', 'delivery'].includes(String(data.recordKind)) && text('id', 64) && /^[0-9a-f]{64}$/.test(data.id as string) && exact('kind', 'repository', 'recordKind', 'id')) return data as Document;
  if (data.kind === 'review' && identity('actor') && identity('request') && text('epoch', 20) && /^[1-9][0-9]*$/.test(data.epoch as string) && exact('kind', 'repository', 'actor', 'request', 'epoch')) return data as Document;
}

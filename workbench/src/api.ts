export type Mode = 'self_hosted' | 'cloud';
export type User = { actor_id: string; display_name: string; administrator: boolean };
export type Session = {
  installation_id: string; workspace_id: string; deployment_mode: Mode;
  runtime_generation: string; build_id: string; user: User; csrf: string;
  products: { product: 'alt' | 'sentori'; title: string; url: string }[];
};
export type Scope = { installation_id: string; repository_instance_id: string; recovery_epoch: string };
export type Repository = { scope: Scope; default_branch: string; writes_enabled: boolean; can_review: boolean; can_publish: boolean };
export type Ref = { name: string; oid: string };
export type Page<T> = { items: T[]; next: string | null };
export type RecordKind = 'task' | 'candidate' | 'evidence' | 'review' | 'integration' | 'delivery';
export type RecordSummary = { kind: RecordKind; id: string; title: string; published_by: string; published_at: string; task_id: string | null };
export type RequestKey = { scope: Scope; actor_id: string; request_id: string };
export type RequestSummary = { key: RequestKey; state: string; digest: string; roots: { kind: RecordKind; id: string }[] };
export type ApprovalContext = {
  request_digest: string; runtime_generation: string; policy_version: string;
  collector_generation: string; revocation_generation: string;
  target_refs: Record<string, string | null>;
};
export type Review = {
  key: RequestKey; inspected: ApprovalContext; integration_id: string;
  candidate_id: string; historical_reviewer: string;
  changes: { path: string; kind: string; before: string | null; after: string | null; patch: string | null }[];
  evidence: { id: string; command: string; outcome: string; source: string; accepted: boolean; reason: string }[];
  changes_truncated: boolean; original_changes_url: string;
  approved: boolean; eligible: boolean; reason: string;
};
export type Tree = { commit: string; path: string; display_path: string; parent: string | null; entries: { name: string; path: string; display_path: string; oid: string; kind: 'tree' | 'blob' | 'symlink' | 'gitlink'; bytes: number | null }[] };
export type Blob = { oid: string; bytes: string; text: string | null; truncated: boolean; download_url: string };
export type Commit = { oid: string; parents: string[]; author: string; timestamp: string; message: string };
export type RecordDetail = {
  fields_truncated: boolean; summary: RecordSummary; fields: { label: string; value: string }[];
  related: { kind: RecordKind; id: string; title: string }[];
  attachments: { name: string; bytes: string; download_url: string | null; omitted_reason: string | null }[];
  publication: { actor_id: string; request_id: string; digest: string; sequence: string };
};
export class ApiError extends Error {
  constructor(readonly status: number, readonly code: string, readonly uncertain: boolean) {
    super(code);
  }
}
export class Api {
  private csrf = '';
  setSession(session: Session | null) { this.csrf = session?.csrf ?? ''; }
  async request<T>(path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
    if (!path.startsWith('/web/') || path.startsWith('//')) throw new Error('invalid API path');
    const writing = body !== undefined;
    let response: Response;
    try {
      response = await fetch(path, {
        method: writing ? 'POST' : 'GET', credentials: 'same-origin', redirect: 'error', cache: 'no-store', signal,
        headers: writing ? { 'Content-Type': 'application/json', 'X-Alt-CSRF': this.csrf } : { Accept: 'application/json' },
        body: writing ? JSON.stringify(body) : undefined,
      });
    } catch (error) {
      if (signal?.aborted) throw error;
      throw new ApiError(0, 'connection', writing);
    }
    // Reading is bounded even if an intermediary omits Content-Length.
    const reader = response.body?.getReader();
    const chunks: Uint8Array[] = [];
    let length = 0;
    if (reader) {
      try {
        while (true) {
          const { value, done } = await reader.read();
          if (done) break;
          length += value.length;
          if (length > 8 * 1024 * 1024) { await reader.cancel(); throw new Error('response limit'); }
          chunks.push(value);
        }
      } catch { throw new ApiError(response.status, 'response', writing); }
    }
    const bytes = new Uint8Array(length);
    let offset = 0;
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
    let value: unknown;
    try { value = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes)); }
    catch { throw new ApiError(response.status, 'response', writing); }
    if (!response.ok) {
      const code = typeof value === 'object' && value !== null && 'code' in value && typeof value.code === 'string' ? value.code : 'request';
      throw new ApiError(response.status, code, writing && (response.status >= 500 || code === 'outcome_unknown'));
    }
    return value as T;
  }
}
export const api = new Api();
export function repoPath(repository: string, suffix = '') { return `/web/repositories/${encodeURIComponent(repository)}${suffix}`; }
export function query(values: Record<string, string | undefined>) {
  const entries = Object.entries(values).filter((entry): entry is [string, string] => entry[1] !== undefined);
  const text = new URLSearchParams(entries).toString();
  return text ? `?${text}` : '';
}

export type InstanceAuditEntry = { sequence:string; actor_id:string; action:string; subject_id:string; occurred_at_unix_ms:string };
export type InstanceStatus = {
  schema:number; observed_at_unix_ms:string; repositories:string; stored_bytes:string; reserved_bytes:string;
  pending_requests:string; recovery_gaps:string; pending_events:string; last_restore:InstanceAuditEntry|null;
  runtime_generation:string; installation_id:string;
};

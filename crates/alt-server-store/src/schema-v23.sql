-- A Git object a ref no longer leads to is given by the digest of its content
-- where a published publication of records names it in its manifest. That is
-- asked of the manifests by containment: without these, of every one of the
-- repository in turn.
CREATE INDEX alt_requests_manifest_entries ON alt_requests USING gin ((manifest->'entries') jsonb_path_ops) WHERE state='committed';
CREATE INDEX alt_archived_publications_manifest_entries ON alt_archived_publications USING gin ((manifest->'entries') jsonb_path_ops);

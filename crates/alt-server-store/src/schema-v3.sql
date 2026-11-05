-- HEAD is repository metadata, not a heuristic over whatever ref happens to
-- sort first. Operators may select an unborn branch explicitly.
ALTER TABLE alt_repositories ADD COLUMN default_branch text NOT NULL DEFAULT 'refs/heads/main';

-- A root may be explicitly republished with another complete selection or
-- attachment disposition. Keep every publication association, not only the
-- first appearance in the distinct-root catalog.
CREATE TABLE alt_task_bundle_roots (
  repository text NOT NULL,
  epoch bigint NOT NULL,
  actor_id text NOT NULL,
  request_id text NOT NULL,
  kind text NOT NULL,
  record_id text NOT NULL,
  PRIMARY KEY(repository,epoch,actor_id,request_id,kind,record_id),
  FOREIGN KEY(repository,epoch,actor_id,request_id) REFERENCES alt_task_bundles(repository,epoch,actor_id,request_id)
);
CREATE INDEX alt_task_bundle_roots_lookup ON alt_task_bundle_roots(repository,kind,record_id);
INSERT INTO alt_task_bundle_roots(repository,epoch,actor_id,request_id,kind,record_id)
SELECT q.repository,q.epoch,q.actor_id,q.request_id,root->>'kind',root->>'id'
FROM alt_requests q JOIN alt_task_bundles b
ON b.repository=q.repository AND b.epoch=q.epoch AND b.actor_id=q.actor_id AND b.request_id=q.request_id
CROSS JOIN LATERAL jsonb_array_elements(q.publication->'record_roots') root
WHERE q.state='committed';

-- Selected dependency records must also be discoverable (for example a Task
-- received as part of an Integration root). The association binds its exact
-- original payload variant to a particular published selection.
CREATE TABLE alt_task_bundle_records (
  repository text NOT NULL,
  epoch bigint NOT NULL,
  actor_id text NOT NULL,
  request_id text NOT NULL,
  kind text NOT NULL,
  record_id text NOT NULL,
  payload_blake3 text NOT NULL,
  PRIMARY KEY(repository,epoch,actor_id,request_id,kind,record_id),
  FOREIGN KEY(repository,epoch,actor_id,request_id) REFERENCES alt_task_bundles(repository,epoch,actor_id,request_id),
  FOREIGN KEY(repository,kind,record_id,payload_blake3) REFERENCES alt_task_records(repository,kind,record_id,payload_blake3)
);
CREATE INDEX alt_task_bundle_records_lookup ON alt_task_bundle_records(repository,kind,record_id);
INSERT INTO alt_task_bundle_records(repository,epoch,actor_id,request_id,kind,record_id,payload_blake3)
SELECT q.repository,q.epoch,q.actor_id,q.request_id,r.kind,r.record_id,r.payload_blake3
FROM alt_requests q JOIN alt_task_bundles b
ON b.repository=q.repository AND b.epoch=q.epoch AND b.actor_id=q.actor_id AND b.request_id=q.request_id
CROSS JOIN LATERAL jsonb_array_elements(q.manifest->'entries') entry
JOIN alt_task_records r ON r.repository=q.repository AND r.payload_blake3=entry->>'blake3'
WHERE q.state='committed' AND entry->>'kind'='record';

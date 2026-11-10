-- Physical locations are subordinate to the existing logical blob catalog.
-- A committed bounded intent owns files even if installation/registration dies.
CREATE TABLE alt_segments (
    repository text NOT NULL REFERENCES alt_repositories(id),
    segment_id text NOT NULL CHECK (segment_id ~ '^[0-9a-f]{32}$'),
    owner_kind text NOT NULL CHECK (owner_kind IN ('upload','compact')),
    epoch bigint,
    actor_id text,
    request_id text,
    state text NOT NULL CHECK (state IN ('uploading','stored','deleting')),
    expires_at bigint NOT NULL CHECK (expires_at > 0),
    member_count bigint NOT NULL CHECK (member_count BETWEEN 1 AND 8192),
    payload_bytes bigint NOT NULL CHECK (payload_bytes BETWEEN 0 AND 67108864),
    selection jsonb NOT NULL,
    index_digest text CHECK (index_digest ~ '^[0-9a-f]{64}$'),
    PRIMARY KEY(repository,segment_id),
    FOREIGN KEY(repository,epoch,actor_id,request_id)
      REFERENCES alt_requests(repository,epoch,actor_id,request_id),
    CHECK ((owner_kind='upload' AND epoch IS NOT NULL AND actor_id IS NOT NULL AND request_id IS NOT NULL)
        OR (owner_kind='compact' AND epoch IS NULL AND actor_id IS NULL AND request_id IS NULL)),
    CHECK (state<>'stored' OR index_digest IS NOT NULL)
);
CREATE INDEX alt_segments_owner ON alt_segments(repository,epoch,actor_id,request_id) WHERE state='uploading';
CREATE TABLE alt_blob_locations (
    repository text NOT NULL,
    blake3 text NOT NULL,
    segment_id text NOT NULL,
    index_digest text NOT NULL CHECK (index_digest ~ '^[0-9a-f]{64}$'),
    byte_offset bigint NOT NULL CHECK (byte_offset >= 64 AND byte_offset < 67502176),
    PRIMARY KEY(repository,blake3),
    FOREIGN KEY(repository,blake3) REFERENCES alt_blobs(repository,blake3) ON DELETE CASCADE,
    FOREIGN KEY(repository,segment_id) REFERENCES alt_segments(repository,segment_id)
);
CREATE INDEX alt_blob_locations_segment ON alt_blob_locations(repository,segment_id);

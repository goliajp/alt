-- Git objects are stored as verified immutable pack segments: one row per
-- segment, object locations live in the segment's own index.
CREATE TABLE alt_pack_segments (
  repository text NOT NULL REFERENCES alt_repositories(id),
  segment_id text NOT NULL,
  sequence bigint NOT NULL CHECK (sequence > 0),
  objects bigint NOT NULL CHECK (objects > 0),
  bytes bigint NOT NULL CHECK (bytes > 0),
  flags integer NOT NULL,
  state text NOT NULL CHECK (state IN ('live','retired')),
  retired_at bigint,
  -- a small segment's whole file, made durable by the publishing commit itself;
  -- the file on disk is then only a cache until the segment is settled
  inline bytea,
  PRIMARY KEY(repository, segment_id)
);
CREATE INDEX alt_pack_segments_live ON alt_pack_segments(repository, sequence) WHERE state='live';
-- set once a ref was deleted or moved off its history: some stored objects may
-- no longer be reachable, so whole segments are not resent until compaction
ALTER TABLE alt_repositories ADD COLUMN pack_unreachable boolean NOT NULL DEFAULT false;

-- Indexed projections of an immutable intent. The original manifest remains
-- authoritative history; progress and these selections share its request key.
ALTER TABLE alt_request_content
  ADD COLUMN selected_entries jsonb NOT NULL DEFAULT '[]'::jsonb,
  ADD COLUMN chunk_bytes bigint;

-- Expand each original manifest once, including equal bytes in several roles.
-- A content digest can also occur as a chunk; neither role shadows the other.
CREATE TEMP TABLE alt_upload_selection_backfill ON COMMIT DROP AS
WITH roots AS MATERIALIZED (
  SELECT r.repository,r.epoch,r.actor_id,r.request_id,e.entry,e.ordinality
  FROM alt_requests r
  CROSS JOIN LATERAL jsonb_array_elements(r.manifest->'entries')
    WITH ORDINALITY AS e(entry,ordinality)
), occurrences AS (
  SELECT repository,epoch,actor_id,request_id,entry->>'blake3' AS blake3,
    (entry->>'bytes')::bigint AS bytes,entry,ordinality
  FROM roots
  UNION ALL
  SELECT repository,epoch,actor_id,request_id,c.chunk->>'blake3',
    (c.chunk->>'bytes')::bigint,NULL::jsonb,NULL::bigint
  FROM roots
  CROSS JOIN LATERAL jsonb_array_elements(entry->'chunks') AS c(chunk)
)
SELECT repository,epoch,actor_id,request_id,blake3,min(bytes) AS bytes,
  max(bytes) AS max_bytes,
  coalesce(jsonb_agg(entry ORDER BY ordinality) FILTER (WHERE entry IS NOT NULL),'[]'::jsonb) AS entries,
  bool_or(entry IS NULL) AS is_chunk
FROM occurrences
GROUP BY repository,epoch,actor_id,request_id,blake3;

DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM alt_upload_selection_backfill WHERE bytes<>max_bytes OR bytes<0)
    OR EXISTS (
      SELECT 1 FROM alt_upload_selection_backfill s
      JOIN alt_request_content c USING(repository,epoch,actor_id,request_id,blake3)
      JOIN alt_blobs b USING(repository,blake3)
      WHERE s.bytes<>b.bytes
    ) THEN
    RAISE EXCEPTION 'conflicting content sizes in original upload manifest';
  END IF;
END $$;

UPDATE alt_request_content c
SET selected_entries=s.entries,
    chunk_bytes=CASE WHEN s.is_chunk THEN s.bytes ELSE NULL END
FROM alt_upload_selection_backfill s
WHERE c.repository=s.repository AND c.epoch=s.epoch AND c.actor_id=s.actor_id
  AND c.request_id=s.request_id AND c.blake3=s.blake3;

ALTER TABLE alt_request_content
  ADD CONSTRAINT alt_upload_selection_array CHECK (jsonb_typeof(selected_entries)='array'),
  ADD CONSTRAINT alt_upload_selection_present CHECK (selected_entries<>'[]'::jsonb OR chunk_bytes IS NOT NULL),
  ADD CONSTRAINT alt_upload_selection_chunk_size CHECK (chunk_bytes>0);

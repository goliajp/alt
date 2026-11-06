-- Snapshot intent and retention become durable before expensive validation.
-- Only a completed validation can expose the signed envelope for download.
ALTER TABLE alt_portable_exports ADD COLUMN verified_at bigint CHECK(verified_at>=0);
-- Earlier versions inserted the row only after full validation succeeded.
UPDATE alt_portable_exports SET verified_at=created_at;

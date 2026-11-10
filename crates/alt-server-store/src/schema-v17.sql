-- State/verified updates do not change indexed keys. Reserve heap-page room
-- for HOT updates rather than rewriting every index entry. Existing rows and
-- immutable payloads are not rewritten; this controls future page placement.
ALTER TABLE alt_blobs SET (fillfactor=50);
ALTER TABLE alt_request_content SET (fillfactor=50);
